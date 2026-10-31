//! In-memory CQL engine for the embedded Cassandra server: a CQL subset parser
//! (`CREATE/DROP KEYSPACE`, `CREATE/DROP TABLE`, `TRUNCATE`, `USE`, `INSERT ... [IF NOT
//! EXISTS] [USING TTL]`, `UPDATE ... SET` (incl. counters and collection `+`/`-`),
//! `DELETE`, `SELECT [DISTINCT] cols|*|COUNT(*) ... WHERE ... [ORDER BY] [LIMIT] [ALLOW
//! FILTERING]`), bind markers with type inference for `PREPARE`, and execution with
//! Cassandra's primary key restrictions.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use camel_core::value::{compare_values, Value};

use super::frame::{error_code, format_uuid, parse_uuid, random_uuid, time_uuid, timestamp_millis, ColumnSpec, CqlType};
use crate::mongodb::bson::format_iso_date;
use crate::util::unhex;

/// An engine error: (CQL error code, message).
#[derive(Clone, Debug, PartialEq)]
pub struct CqlError(pub i32, pub String);

type R<T> = std::result::Result<T, CqlError>;

fn syntax(m: impl Into<String>) -> CqlError {
    CqlError(error_code::SYNTAX_ERROR, m.into())
}

fn invalid(m: impl Into<String>) -> CqlError {
    CqlError(error_code::INVALID, m.into())
}

// ---------------------------------------------------------------------------
// lexer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String, bool),
    Str(String),
    Num(String),
    Uuid(String),
    Blob(Vec<u8>),
    Sym(String),
    Bind,
}

fn is_uuid_at(s: &[u8], i: usize) -> bool {
    let pat = [8, 4, 4, 4, 12];
    let mut p = i;
    for (n, len) in pat.iter().enumerate() {
        for _ in 0..*len {
            if p >= s.len() || !s[p].is_ascii_hexdigit() {
                return false;
            }
            p += 1;
        }
        if n < 4 {
            if p >= s.len() || s[p] != b'-' {
                return false;
            }
            p += 1;
        }
    }
    p >= s.len() || !(s[p].is_ascii_alphanumeric() || s[p] == b'_')
}

fn lex(q: &str) -> R<Vec<Tok>> {
    let s = q.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < s.len() {
        let c = s[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'-' && s.get(i + 1) == Some(&b'-') || c == b'/' && s.get(i + 1) == Some(&b'/') {
            while i < s.len() && s[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if (c.is_ascii_hexdigit()) && is_uuid_at(s, i) {
            out.push(Tok::Uuid(q[i..i + 36].to_ascii_lowercase()));
            i += 36;
            continue;
        }
        if c == b'0' && matches!(s.get(i + 1), Some(b'x') | Some(b'X')) {
            let start = i + 2;
            i = start;
            while i < s.len() && s[i].is_ascii_hexdigit() {
                i += 1;
            }
            out.push(Tok::Blob(unhex(&q[start..i]).ok_or_else(|| syntax("invalid blob literal"))?));
            continue;
        }
        if c.is_ascii_digit() || (c == b'-' && s.get(i + 1).is_some_and(|d| d.is_ascii_digit()) && !matches!(out.last(), Some(Tok::Ident(..)) | Some(Tok::Num(_)) | Some(Tok::Str(_)) | Some(Tok::Bind))) {
            let start = i;
            i += 1;
            while i < s.len() && (s[i].is_ascii_digit() || s[i] == b'.' || s[i] == b'e' || s[i] == b'E' || ((s[i] == b'-' || s[i] == b'+') && matches!(s[i - 1], b'e' | b'E'))) {
                i += 1;
            }
            out.push(Tok::Num(q[start..i].to_string()));
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < s.len() && (s[i].is_ascii_alphanumeric() || s[i] == b'_') {
                i += 1;
            }
            out.push(Tok::Ident(q[start..i].to_ascii_lowercase(), false));
            continue;
        }
        match c {
            b'\'' => {
                let mut v = String::new();
                i += 1;
                loop {
                    let Some(&ch) = s.get(i) else { return Err(syntax("unterminated string literal")) };
                    if ch == b'\'' {
                        if s.get(i + 1) == Some(&b'\'') {
                            v.push('\'');
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    let len = utf8_len(ch);
                    v.push_str(&q[i..i + len]);
                    i += len;
                }
                out.push(Tok::Str(v));
            }
            b'"' => {
                let mut v = String::new();
                i += 1;
                loop {
                    let Some(&ch) = s.get(i) else { return Err(syntax("unterminated quoted identifier")) };
                    if ch == b'"' {
                        if s.get(i + 1) == Some(&b'"') {
                            v.push('"');
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    let len = utf8_len(ch);
                    v.push_str(&q[i..i + len]);
                    i += len;
                }
                out.push(Tok::Ident(v, true));
            }
            b'?' => {
                out.push(Tok::Bind);
                i += 1;
            }
            b'<' | b'>' | b'!' => {
                if s.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Sym(q[i..i + 2].to_string()));
                    i += 2;
                } else {
                    out.push(Tok::Sym((c as char).to_string()));
                    i += 1;
                }
            }
            b'(' | b')' | b',' | b';' | b'=' | b'*' | b'.' | b'[' | b']' | b'{' | b'}' | b':' | b'+' | b'-' => {
                out.push(Tok::Sym((c as char).to_string()));
                i += 1;
            }
            _ => return Err(syntax(format!("line 1:{} no viable alternative at character '{}'", i, c as char))),
        }
    }
    Ok(out)
}

fn utf8_len(b: u8) -> usize {
    match b {
        0xf0..=0xff => 4,
        0xe0..=0xef => 3,
        0xc0..=0xdf => 2,
        _ => 1,
    }
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

/// A term (literal, bind marker, collection literal or function call).
#[derive(Clone, Debug, PartialEq)]
pub enum Term {
    Lit(Value),
    Bind(usize),
    List(Vec<Term>),
    Set(Vec<Term>),
    Map(Vec<(Term, Term)>),
    Func(String, Vec<Term>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    Contains,
    ContainsKey,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cond {
    pub col: String,
    pub op: Op,
    pub term: Term,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Assign {
    Set(String, Term),
    Add(String, Term),
    Sub(String, Term),
    Prepend(String, Term),
    Index(String, Term, Term),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Selection {
    All,
    Count(String),
    Cols(Vec<(String, Option<String>)>),
}

/// A qualified table name.
#[derive(Clone, Debug, PartialEq)]
pub struct TableRef {
    pub keyspace: Option<String>,
    pub name: String,
}

/// A parsed statement.
#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    CreateKeyspace { name: String, if_not_exists: bool },
    DropKeyspace { name: String, if_exists: bool },
    Use(String),
    CreateTable { table: TableRef, cols: Vec<(String, CqlType)>, partition: Vec<String>, clustering: Vec<String>, if_not_exists: bool },
    DropTable { table: TableRef, if_exists: bool },
    Truncate(TableRef),
    Insert { table: TableRef, cols: Vec<String>, values: Vec<Term>, if_not_exists: bool, ttl: Option<Term> },
    Update { table: TableRef, sets: Vec<Assign>, conds: Vec<Cond>, if_exists: bool, ttl: Option<Term> },
    Delete { table: TableRef, cols: Vec<String>, conds: Vec<Cond>, if_exists: bool },
    Select { table: TableRef, sel: Selection, conds: Vec<Cond>, order: Option<(String, bool)>, limit: Option<Term>, allow_filtering: bool, distinct: bool },
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    binds: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(s, false)) if s == kw)
    }
    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect_kw(&mut self, kw: &str) -> R<()> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(syntax(format!("line 1:{} mismatched input {:?} expecting {}", self.pos, self.peek(), kw.to_uppercase())))
        }
    }
    fn is_sym(&self, s: &str) -> bool {
        matches!(self.peek(), Some(Tok::Sym(x)) if x == s)
    }
    fn eat_sym(&mut self, s: &str) -> bool {
        if self.is_sym(s) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect_sym(&mut self, s: &str) -> R<()> {
        if self.eat_sym(s) {
            Ok(())
        } else {
            Err(syntax(format!("line 1:{} mismatched input {:?} expecting '{}'", self.pos, self.peek(), s)))
        }
    }
    fn ident(&mut self) -> R<String> {
        match self.next() {
            Some(Tok::Ident(s, _)) => Ok(s),
            other => Err(syntax(format!("expected identifier, got {:?}", other))),
        }
    }
    fn if_not_exists(&mut self) -> R<bool> {
        if self.eat_kw("if") {
            self.expect_kw("not")?;
            self.expect_kw("exists")?;
            return Ok(true);
        }
        Ok(false)
    }
    fn if_exists(&mut self) -> R<bool> {
        if self.eat_kw("if") {
            self.expect_kw("exists")?;
            return Ok(true);
        }
        Ok(false)
    }
    fn table_ref(&mut self) -> R<TableRef> {
        let first = self.ident()?;
        if self.eat_sym(".") {
            Ok(TableRef { keyspace: Some(first), name: self.ident()? })
        } else {
            Ok(TableRef { keyspace: None, name: first })
        }
    }
    fn skip_rest(&mut self) {
        self.pos = self.toks.len();
    }
    fn cql_type(&mut self) -> R<CqlType> {
        let name = self.ident()?;
        let mut text = name.clone();
        if self.eat_sym("<") {
            let mut depth = 1;
            text.push('<');
            while depth > 0 {
                match self.next() {
                    Some(Tok::Sym(s)) if s == "<" => {
                        depth += 1;
                        text.push('<')
                    }
                    Some(Tok::Sym(s)) if s == ">" => {
                        depth -= 1;
                        text.push('>')
                    }
                    Some(Tok::Sym(s)) if s == "," => text.push(','),
                    Some(Tok::Ident(s, _)) => text.push_str(&s),
                    other => return Err(syntax(format!("invalid type near {:?}", other))),
                }
            }
        }
        CqlType::parse(&text).ok_or_else(|| invalid(format!("Unknown type {}", text)))
    }
    fn term(&mut self) -> R<Term> {
        match self.next() {
            Some(Tok::Bind) => {
                self.binds += 1;
                Ok(Term::Bind(self.binds - 1))
            }
            Some(Tok::Str(s)) => Ok(Term::Lit(Value::String(s))),
            Some(Tok::Num(n)) => Ok(Term::Lit(if n.contains(['.', 'e', 'E']) { Value::Float(n.parse().map_err(|_| syntax("invalid number"))?) } else { Value::Int(n.parse().map_err(|_| syntax("invalid integer"))?) })),
            Some(Tok::Sym(s)) if s == "-" => match self.term()? {
                Term::Lit(Value::Int(i)) => Ok(Term::Lit(Value::Int(-i))),
                Term::Lit(Value::Float(f)) => Ok(Term::Lit(Value::Float(-f))),
                _ => Err(syntax("invalid negative literal")),
            },
            Some(Tok::Uuid(u)) => Ok(Term::Lit(Value::String(u))),
            Some(Tok::Blob(b)) => Ok(Term::Lit(Value::Bytes(b))),
            Some(Tok::Ident(s, false)) if s == "true" || s == "false" => Ok(Term::Lit(Value::Bool(s == "true"))),
            Some(Tok::Ident(s, false)) if s == "null" => Ok(Term::Lit(Value::Null)),
            Some(Tok::Ident(f, false)) if self.is_sym("(") => {
                self.pos += 1;
                let mut args = Vec::new();
                if !self.eat_sym(")") {
                    loop {
                        args.push(self.term()?);
                        if self.eat_sym(")") {
                            break;
                        }
                        self.expect_sym(",")?;
                    }
                }
                Ok(Term::Func(f, args))
            }
            Some(Tok::Sym(s)) if s == "[" => {
                let mut items = Vec::new();
                if !self.eat_sym("]") {
                    loop {
                        items.push(self.term()?);
                        if self.eat_sym("]") {
                            break;
                        }
                        self.expect_sym(",")?;
                    }
                }
                Ok(Term::List(items))
            }
            Some(Tok::Sym(s)) if s == "{" => {
                if self.eat_sym("}") {
                    return Ok(Term::Map(Vec::new()));
                }
                let first = self.term()?;
                if self.eat_sym(":") {
                    let mut pairs = vec![(first, self.term()?)];
                    while self.eat_sym(",") {
                        let k = self.term()?;
                        self.expect_sym(":")?;
                        pairs.push((k, self.term()?));
                    }
                    self.expect_sym("}")?;
                    Ok(Term::Map(pairs))
                } else {
                    let mut items = vec![first];
                    while self.eat_sym(",") {
                        items.push(self.term()?);
                    }
                    self.expect_sym("}")?;
                    Ok(Term::Set(items))
                }
            }
            Some(Tok::Sym(s)) if s == "(" => {
                // tuple literal
                let mut items = Vec::new();
                loop {
                    items.push(self.term()?);
                    if self.eat_sym(")") {
                        break;
                    }
                    self.expect_sym(",")?;
                }
                Ok(Term::List(items))
            }
            other => Err(syntax(format!("line 1:{} no viable alternative at input {:?}", self.pos, other))),
        }
    }
    fn conds(&mut self) -> R<Vec<Cond>> {
        let mut out = Vec::new();
        loop {
            let col = self.ident()?;
            let op = match self.next() {
                Some(Tok::Sym(s)) => match s.as_str() {
                    "=" => Op::Eq,
                    "!=" => Op::Ne,
                    "<" => Op::Lt,
                    "<=" => Op::Le,
                    ">" => Op::Gt,
                    ">=" => Op::Ge,
                    _ => return Err(syntax(format!("invalid operator {}", s))),
                },
                Some(Tok::Ident(s, false)) if s == "in" => Op::In,
                Some(Tok::Ident(s, false)) if s == "contains" => {
                    if self.eat_kw("key") {
                        Op::ContainsKey
                    } else {
                        Op::Contains
                    }
                }
                other => return Err(syntax(format!("invalid relation near {:?}", other))),
            };
            let term = if op == Op::In && self.eat_sym("(") {
                let mut items = Vec::new();
                if !self.eat_sym(")") {
                    loop {
                        items.push(self.term()?);
                        if self.eat_sym(")") {
                            break;
                        }
                        self.expect_sym(",")?;
                    }
                }
                Term::List(items)
            } else {
                self.term()?
            };
            out.push(Cond { col, op, term });
            if !self.eat_kw("and") {
                break;
            }
        }
        Ok(out)
    }
    fn using(&mut self) -> R<Option<Term>> {
        let mut ttl = None;
        if self.eat_kw("using") {
            loop {
                if self.eat_kw("ttl") {
                    ttl = Some(self.term()?);
                } else if self.eat_kw("timestamp") {
                    self.term()?;
                } else {
                    return Err(syntax("expected TTL or TIMESTAMP"));
                }
                if !self.eat_kw("and") {
                    break;
                }
            }
        }
        Ok(ttl)
    }
    fn end(&mut self) -> R<()> {
        self.eat_sym(";");
        if self.pos < self.toks.len() {
            return Err(syntax(format!("line 1:{} extraneous input {:?}", self.pos, self.peek())));
        }
        Ok(())
    }
}

/// Parses one statement; returns it and the number of bind markers.
pub fn parse(q: &str) -> R<(Stmt, usize)> {
    let mut p = Parser { toks: lex(q)?, pos: 0, binds: 0 };
    let kw = p.ident()?;
    let stmt = match kw.as_str() {
        "create" => {
            if p.eat_kw("keyspace") || p.eat_kw("schema") {
                let ine = p.if_not_exists()?;
                let name = p.ident()?;
                p.skip_rest();
                Stmt::CreateKeyspace { name, if_not_exists: ine }
            } else if p.eat_kw("table") || p.eat_kw("columnfamily") {
                let ine = p.if_not_exists()?;
                let table = p.table_ref()?;
                p.expect_sym("(")?;
                let mut cols = Vec::new();
                let mut partition = Vec::new();
                let mut clustering = Vec::new();
                loop {
                    if p.eat_kw("primary") {
                        p.expect_kw("key")?;
                        p.expect_sym("(")?;
                        if p.eat_sym("(") {
                            loop {
                                partition.push(p.ident()?);
                                if p.eat_sym(")") {
                                    break;
                                }
                                p.expect_sym(",")?;
                            }
                        } else {
                            partition.push(p.ident()?);
                        }
                        while p.eat_sym(",") {
                            clustering.push(p.ident()?);
                        }
                        p.expect_sym(")")?;
                    } else {
                        let name = p.ident()?;
                        let t = p.cql_type()?;
                        if p.eat_kw("static") {}
                        if p.eat_kw("primary") {
                            p.expect_kw("key")?;
                            partition = vec![name.clone()];
                        }
                        cols.push((name, t));
                    }
                    if p.eat_sym(")") {
                        break;
                    }
                    p.expect_sym(",")?;
                }
                if partition.is_empty() {
                    return Err(invalid("No PRIMARY KEY specified (exactly one required)"));
                }
                if p.eat_kw("with") {
                    p.skip_rest();
                }
                Stmt::CreateTable { table, cols, partition, clustering, if_not_exists: ine }
            } else if p.eat_kw("index") || p.eat_kw("type") || p.eat_kw("custom") {
                return Err(invalid("CREATE INDEX/TYPE is not supported by the embedded server"));
            } else {
                return Err(syntax("expected KEYSPACE or TABLE after CREATE"));
            }
        }
        "drop" => {
            if p.eat_kw("keyspace") || p.eat_kw("schema") {
                let ie = p.if_exists()?;
                Stmt::DropKeyspace { name: p.ident()?, if_exists: ie }
            } else if p.eat_kw("table") || p.eat_kw("columnfamily") {
                let ie = p.if_exists()?;
                Stmt::DropTable { table: p.table_ref()?, if_exists: ie }
            } else {
                return Err(syntax("expected KEYSPACE or TABLE after DROP"));
            }
        }
        "truncate" => {
            p.eat_kw("table");
            Stmt::Truncate(p.table_ref()?)
        }
        "use" => Stmt::Use(p.ident()?),
        "insert" => {
            p.expect_kw("into")?;
            let table = p.table_ref()?;
            if p.eat_kw("json") {
                return Err(invalid("INSERT JSON is not supported by the embedded server"));
            }
            p.expect_sym("(")?;
            let mut cols = Vec::new();
            loop {
                cols.push(p.ident()?);
                if p.eat_sym(")") {
                    break;
                }
                p.expect_sym(",")?;
            }
            p.expect_kw("values")?;
            p.expect_sym("(")?;
            let mut values = Vec::new();
            loop {
                values.push(p.term()?);
                if p.eat_sym(")") {
                    break;
                }
                p.expect_sym(",")?;
            }
            if cols.len() != values.len() {
                return Err(invalid(format!("Unmatched column names/values: {} columns, {} values", cols.len(), values.len())));
            }
            let ine = p.if_not_exists()?;
            let ttl = p.using()?;
            Stmt::Insert { table, cols, values, if_not_exists: ine, ttl }
        }
        "update" => {
            let table = p.table_ref()?;
            let ttl = p.using()?;
            p.expect_kw("set")?;
            let mut sets = Vec::new();
            loop {
                let col = p.ident()?;
                if p.eat_sym("[") {
                    let k = p.term()?;
                    p.expect_sym("]")?;
                    p.expect_sym("=")?;
                    sets.push(Assign::Index(col, k, p.term()?));
                } else {
                    p.expect_sym("=")?;
                    let is_self = matches!(p.peek(), Some(Tok::Ident(s, _)) if *s == col) && matches!(p.toks.get(p.pos + 1), Some(Tok::Sym(s)) if s == "+" || s == "-");
                    if is_self {
                        p.pos += 1;
                        let plus = p.eat_sym("+");
                        if !plus {
                            p.expect_sym("-")?;
                        }
                        let t = p.term()?;
                        sets.push(if plus { Assign::Add(col, t) } else { Assign::Sub(col, t) });
                    } else {
                        let t = p.term()?;
                        if p.eat_sym("+") {
                            let c2 = p.ident()?;
                            if c2 != col {
                                return Err(invalid(format!("Only expressions of the form X = <value> + X are supported for {}", col)));
                            }
                            sets.push(Assign::Prepend(col, t));
                        } else {
                            sets.push(Assign::Set(col, t));
                        }
                    }
                }
                if !p.eat_sym(",") {
                    break;
                }
            }
            p.expect_kw("where")?;
            let conds = p.conds()?;
            let ie = p.if_exists()?;
            Stmt::Update { table, sets, conds, if_exists: ie, ttl }
        }
        "delete" => {
            let mut cols = Vec::new();
            if !p.is_kw("from") {
                loop {
                    cols.push(p.ident()?);
                    if !p.eat_sym(",") {
                        break;
                    }
                }
            }
            p.expect_kw("from")?;
            let table = p.table_ref()?;
            p.using()?;
            p.expect_kw("where")?;
            let conds = p.conds()?;
            let ie = p.if_exists()?;
            Stmt::Delete { table, cols, conds, if_exists: ie }
        }
        "select" => {
            if p.eat_kw("json") {
                return Err(invalid("SELECT JSON is not supported by the embedded server"));
            }
            let distinct = p.eat_kw("distinct");
            let sel = if p.eat_sym("*") {
                Selection::All
            } else if p.is_kw("count") && matches!(p.toks.get(p.pos + 1), Some(Tok::Sym(s)) if s == "(") {
                p.pos += 2;
                if !p.eat_sym("*") {
                    p.term()?;
                }
                p.expect_sym(")")?;
                let alias = if p.eat_kw("as") { p.ident()? } else { "count".into() };
                Selection::Count(alias)
            } else {
                let mut cols = Vec::new();
                loop {
                    let c = p.ident()?;
                    let alias = if p.eat_kw("as") { Some(p.ident()?) } else { None };
                    cols.push((c, alias));
                    if !p.eat_sym(",") {
                        break;
                    }
                }
                Selection::Cols(cols)
            };
            p.expect_kw("from")?;
            let table = p.table_ref()?;
            let conds = if p.eat_kw("where") { p.conds()? } else { Vec::new() };
            let mut order = None;
            if p.eat_kw("order") {
                p.expect_kw("by")?;
                let c = p.ident()?;
                let desc = p.eat_kw("desc");
                if !desc {
                    p.eat_kw("asc");
                }
                order = Some((c, desc));
            }
            let limit = if p.eat_kw("limit") { Some(p.term()?) } else { None };
            let allow = if p.eat_kw("allow") {
                p.expect_kw("filtering")?;
                true
            } else {
                false
            };
            Stmt::Select { table, sel, conds, order, limit, allow_filtering: allow, distinct }
        }
        other => return Err(syntax(format!("line 1:0 no viable alternative at input '{}'", other))),
    };
    p.end()?;
    Ok((stmt, p.binds))
}

// ---------------------------------------------------------------------------
// storage
// ---------------------------------------------------------------------------

type Row = BTreeMap<String, Value>;

/// A table.
#[derive(Clone, Debug)]
pub struct Table {
    pub cols: Vec<(String, CqlType)>,
    pub partition: Vec<String>,
    pub clustering: Vec<String>,
    rows: Vec<(Row, Option<Instant>)>,
}

impl Table {
    fn col_type(&self, c: &str) -> R<&CqlType> {
        self.cols.iter().find(|(n, _)| n == c).map(|(_, t)| t).ok_or_else(|| invalid(format!("Undefined column name {}", c)))
    }
    fn is_pk(&self, c: &str) -> bool {
        self.partition.iter().any(|x| x == c) || self.clustering.iter().any(|x| x == c)
    }
    fn purge(&mut self) {
        let now = Instant::now();
        self.rows.retain(|(_, e)| e.is_none_or(|t| t > now));
    }
    fn key_of(&self, r: &Row) -> Vec<Value> {
        self.partition.iter().chain(self.clustering.iter()).map(|c| r.get(c).cloned().unwrap_or(Value::Null)).collect()
    }
}

/// Converts a value to its stored form for a column type.
pub fn normalize(t: &CqlType, v: &Value) -> R<Value> {
    if v.is_null() {
        return Ok(Value::Null);
    }
    let bad = || invalid(format!("Invalid {} constant ({}) for type {}", v.type_name().to_uppercase(), v, t.name()));
    Ok(match t {
        CqlType::Int | CqlType::Bigint | CqlType::Counter | CqlType::Smallint | CqlType::Tinyint | CqlType::Varint | CqlType::Time => {
            let i = match v {
                Value::Int(i) => *i,
                Value::String(s) if matches!(t, CqlType::Varint) => return Ok(Value::String(s.clone())),
                _ => return Err(bad()),
            };
            let ok = match t {
                CqlType::Int => i32::try_from(i).is_ok(),
                CqlType::Smallint => i16::try_from(i).is_ok(),
                CqlType::Tinyint => i8::try_from(i).is_ok(),
                _ => true,
            };
            if !ok {
                return Err(invalid(format!("Invalid value {} for type {}", i, t.name())));
            }
            Value::Int(i)
        }
        CqlType::Varchar | CqlType::Ascii => match v {
            Value::String(_) => v.clone(),
            Value::Bytes(b) => Value::String(String::from_utf8_lossy(b).into_owned()),
            _ => return Err(bad()),
        },
        CqlType::Boolean => match v {
            Value::Bool(_) => v.clone(),
            _ => return Err(bad()),
        },
        CqlType::Double | CqlType::Float => match v {
            Value::Float(_) => v.clone(),
            Value::Int(i) => Value::Float(*i as f64),
            _ => return Err(bad()),
        },
        CqlType::Decimal => Value::String(v.to_string()),
        CqlType::Timestamp => Value::Int(timestamp_millis(v).ok_or_else(bad)?),
        CqlType::Uuid | CqlType::Timeuuid => Value::String(format_uuid(&parse_uuid(&v.to_string()).ok_or_else(bad)?)),
        CqlType::Blob => match v {
            Value::Bytes(_) => v.clone(),
            _ => return Err(bad()),
        },
        CqlType::Date => match v {
            Value::Int(d) => Value::String(format_iso_date(d * 86_400_000)[..10].to_string()),
            Value::String(s) => Value::String(s.clone()),
            _ => return Err(bad()),
        },
        CqlType::Inet => Value::String(v.to_string()),
        CqlType::List(e) => Value::Array(v.as_array().ok_or_else(bad)?.iter().map(|x| normalize(e, x)).collect::<R<_>>()?),
        CqlType::Set(e) => {
            let mut items: Vec<Value> = v.as_array().ok_or_else(bad)?.iter().map(|x| normalize(e, x)).collect::<R<_>>()?;
            items.sort_by(|a, b| compare_values(a, b).unwrap_or(Ordering::Equal));
            items.dedup();
            Value::Array(items)
        }
        CqlType::Map(_, vt) => Value::Object(v.as_object().ok_or_else(bad)?.iter().map(|(k, x)| Ok((k.clone(), normalize(vt, x)?))).collect::<R<_>>()?),
        _ => v.clone(),
    })
}

fn cmp(a: &Value, b: &Value) -> Ordering {
    compare_values(a, b).unwrap_or_else(|| a.to_string().cmp(&b.to_string()))
}

/// Keyspaces and tables.
#[derive(Default)]
pub struct Schema {
    pub keyspaces: BTreeMap<String, BTreeMap<String, Table>>,
}

/// Result of executing a statement.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Void,
    Rows { cols: Vec<ColumnSpec>, rows: Vec<Vec<Value>> },
    SetKeyspace(String),
    SchemaChange { change: &'static str, target: &'static str, keyspace: String, name: String },
}

fn eval(t: &Term, binds: &[Value]) -> R<Value> {
    Ok(match t {
        Term::Lit(v) => v.clone(),
        Term::Bind(i) => binds.get(*i).cloned().ok_or_else(|| invalid(format!("There were {} markers(?) in CQL but {} bound variables", i + 1, binds.len())))?,
        Term::List(items) | Term::Set(items) => Value::Array(items.iter().map(|x| eval(x, binds)).collect::<R<_>>()?),
        Term::Map(pairs) => Value::Object(pairs.iter().map(|(k, v)| Ok((eval(k, binds)?.to_string(), eval(v, binds)?))).collect::<R<_>>()?),
        Term::Func(f, args) => match f.as_str() {
            "now" => Value::String(time_uuid()),
            "uuid" => Value::String(random_uuid()),
            "totimestamp" | "dateof" | "currenttimestamp" => Value::Int(crate::util::now_millis()),
            "textasblob" => Value::Bytes(eval(args.first().unwrap_or(&Term::Lit(Value::Null)), binds)?.to_string().into_bytes()),
            other => return Err(invalid(format!("Unknown function {}", other))),
        },
    })
}

impl Schema {
    fn table_mut(&mut self, t: &TableRef, cur: Option<&str>) -> R<(String, &mut Table)> {
        let ks = t.keyspace.clone().or_else(|| cur.map(String::from)).ok_or_else(|| invalid("No keyspace has been specified. USE a keyspace, or explicitly specify keyspace.tablename"))?;
        let k = self.keyspaces.get_mut(&ks).ok_or_else(|| invalid(format!("Keyspace {} does not exist", ks)))?;
        let tbl = k.get_mut(&t.name).ok_or_else(|| invalid(format!("unconfigured table {}", t.name)))?;
        Ok((ks, tbl))
    }

    /// Bind marker metadata and result metadata of a statement (for PREPARE).
    pub fn prepare(&mut self, stmt: &Stmt, nbinds: usize, cur: Option<&str>) -> R<(Vec<ColumnSpec>, Vec<ColumnSpec>)> {
        let mut specs: Vec<Option<ColumnSpec>> = vec![None; nbinds];
        let spec = |ks: &str, tbl: &str, name: &str, t: CqlType| ColumnSpec { keyspace: ks.into(), table: tbl.into(), name: name.into(), cql_type: t };
        fn assign(specs: &mut [Option<ColumnSpec>], term: &Term, mk: &dyn Fn(CqlType) -> ColumnSpec, t: &CqlType) {
            match term {
                Term::Bind(i) => {
                    if let Some(s) = specs.get_mut(*i) {
                        *s = Some(mk(t.clone()));
                    }
                }
                Term::List(items) | Term::Set(items) => {
                    let et = match t {
                        CqlType::List(e) | CqlType::Set(e) => (**e).clone(),
                        other => other.clone(),
                    };
                    for it in items {
                        assign(specs, it, mk, &et);
                    }
                }
                Term::Map(pairs) => {
                    if let CqlType::Map(k, v) = t {
                        for (a, b) in pairs {
                            assign(specs, a, mk, k);
                            assign(specs, b, mk, v);
                        }
                    }
                }
                _ => {}
            }
        }
        let mut result = Vec::new();
        match stmt {
            Stmt::Insert { table, cols, values, ttl, .. } => {
                let (ks, t) = self.table_mut(table, cur)?;
                for (c, v) in cols.iter().zip(values) {
                    let ty = t.col_type(c)?.clone();
                    assign(&mut specs, v, &|ty| spec(&ks, &table.name, c, ty), &ty);
                }
                if let Some(tt) = ttl {
                    assign(&mut specs, tt, &|ty| spec(&ks, &table.name, "[ttl]", ty), &CqlType::Int);
                }
            }
            Stmt::Update { table, sets, conds, ttl, .. } => {
                let (ks, t) = self.table_mut(table, cur)?;
                if let Some(tt) = ttl {
                    assign(&mut specs, tt, &|ty| spec(&ks, &table.name, "[ttl]", ty), &CqlType::Int);
                }
                for a in sets {
                    match a {
                        Assign::Set(c, v) | Assign::Add(c, v) | Assign::Sub(c, v) | Assign::Prepend(c, v) => {
                            let ty = t.col_type(c)?.clone();
                            assign(&mut specs, v, &|ty| spec(&ks, &table.name, c, ty), &ty);
                        }
                        Assign::Index(c, k, v) => {
                            let ty = t.col_type(c)?.clone();
                            let (kt, vt) = match &ty {
                                CqlType::Map(k, v) => ((**k).clone(), (**v).clone()),
                                CqlType::List(e) => (CqlType::Int, (**e).clone()),
                                other => (other.clone(), other.clone()),
                            };
                            assign(&mut specs, k, &|ty| spec(&ks, &table.name, &format!("key({})", c), ty), &kt);
                            assign(&mut specs, v, &|ty| spec(&ks, &table.name, &format!("value({})", c), ty), &vt);
                        }
                    }
                }
                cond_specs(&mut specs, conds, t, &ks, &table.name)?;
            }
            Stmt::Delete { table, conds, .. } => {
                let (ks, t) = self.table_mut(table, cur)?;
                cond_specs(&mut specs, conds, t, &ks, &table.name)?;
            }
            Stmt::Select { table, sel, conds, limit, .. } => {
                let (ks, t) = self.table_mut(table, cur)?;
                cond_specs(&mut specs, conds, t, &ks, &table.name)?;
                if let Some(l) = limit {
                    assign(&mut specs, l, &|ty| spec(&ks, &table.name, "[limit]", ty), &CqlType::Int);
                }
                result = select_columns(t, sel, &ks, &table.name)?;
            }
            _ => {}
        }
        let binds = specs.into_iter().enumerate().map(|(i, s)| s.unwrap_or_else(|| spec("", "", &format!("[bind{}]", i), CqlType::Blob))).collect();
        Ok((binds, result))
    }

    /// Executes a statement with decoded (and normalized later) bind values.
    pub fn execute(&mut self, stmt: &Stmt, binds: &[Value], cur: Option<&str>) -> R<Outcome> {
        match stmt {
            Stmt::CreateKeyspace { name, if_not_exists } => {
                if self.keyspaces.contains_key(name) {
                    if *if_not_exists {
                        return Ok(Outcome::Void);
                    }
                    return Err(CqlError(error_code::ALREADY_EXISTS, format!("Keyspace {} already exists", name)));
                }
                self.keyspaces.insert(name.clone(), BTreeMap::new());
                Ok(Outcome::SchemaChange { change: "CREATED", target: "KEYSPACE", keyspace: name.clone(), name: String::new() })
            }
            Stmt::DropKeyspace { name, if_exists } => {
                if self.keyspaces.remove(name).is_none() && !*if_exists {
                    return Err(CqlError(error_code::CONFIG_ERROR, format!("Cannot drop non existing keyspace '{}'.", name)));
                }
                Ok(Outcome::SchemaChange { change: "DROPPED", target: "KEYSPACE", keyspace: name.clone(), name: String::new() })
            }
            Stmt::Use(ks) => {
                if !self.keyspaces.contains_key(ks) {
                    return Err(invalid(format!("Keyspace '{}' does not exist", ks)));
                }
                Ok(Outcome::SetKeyspace(ks.clone()))
            }
            Stmt::CreateTable { table, cols, partition, clustering, if_not_exists } => {
                let ks = table.keyspace.clone().or_else(|| cur.map(String::from)).ok_or_else(|| invalid("No keyspace has been specified. USE a keyspace, or explicitly specify keyspace.tablename"))?;
                let k = self.keyspaces.get_mut(&ks).ok_or_else(|| invalid(format!("Keyspace {} doesn't exist", ks)))?;
                if k.contains_key(&table.name) {
                    if *if_not_exists {
                        return Ok(Outcome::Void);
                    }
                    return Err(CqlError(error_code::ALREADY_EXISTS, format!("Object {}.{} already exists", ks, table.name)));
                }
                for c in partition.iter().chain(clustering) {
                    if !cols.iter().any(|(n, _)| n == c) {
                        return Err(invalid(format!("Unknown definition {} referenced in PRIMARY KEY", c)));
                    }
                }
                let regular: Vec<&CqlType> = cols.iter().filter(|(n, _)| !partition.contains(n) && !clustering.contains(n)).map(|(_, t)| t).collect();
                let counters = regular.iter().filter(|t| matches!(t, CqlType::Counter)).count();
                if counters > 0 && counters != regular.len() {
                    return Err(invalid("Cannot mix counter and non counter columns in the same table"));
                }
                k.insert(table.name.clone(), Table { cols: cols.clone(), partition: partition.clone(), clustering: clustering.clone(), rows: Vec::new() });
                Ok(Outcome::SchemaChange { change: "CREATED", target: "TABLE", keyspace: ks, name: table.name.clone() })
            }
            Stmt::DropTable { table, if_exists } => {
                let ks = table.keyspace.clone().or_else(|| cur.map(String::from)).unwrap_or_default();
                let removed = self.keyspaces.get_mut(&ks).and_then(|k| k.remove(&table.name));
                if removed.is_none() && !*if_exists {
                    return Err(invalid(format!("Table '{}.{}' doesn't exist", ks, table.name)));
                }
                Ok(Outcome::SchemaChange { change: "DROPPED", target: "TABLE", keyspace: ks, name: table.name.clone() })
            }
            Stmt::Truncate(table) => {
                let (_, t) = self.table_mut(table, cur)?;
                t.rows.clear();
                Ok(Outcome::Void)
            }
            Stmt::Insert { table, cols, values, if_not_exists, ttl } => {
                let (_, t) = self.table_mut(table, cur)?;
                let mut row = Row::new();
                for (c, v) in cols.iter().zip(values) {
                    let ty = t.col_type(c)?.clone();
                    if matches!(ty, CqlType::Counter) {
                        return Err(invalid("INSERT statements are not allowed on counter tables, use UPDATE instead"));
                    }
                    row.insert(c.clone(), normalize(&ty, &eval(v, binds)?)?);
                }
                for k in &t.partition {
                    if row.get(k).is_none_or(|v| v.is_null()) {
                        return Err(invalid(format!("Some partition key parts are missing: {}", k)));
                    }
                }
                for k in &t.clustering {
                    if row.get(k).is_none_or(|v| v.is_null()) {
                        return Err(invalid(format!("Some clustering keys are missing: {}", k)));
                    }
                }
                let expiry = ttl_expiry(ttl, binds)?;
                let key = t.key_of(&row);
                t.purge();
                let existing = t.rows.iter().position(|(r, _)| t_key(&t.partition, &t.clustering, r) == key);
                if *if_not_exists {
                    if existing.is_some() {
                        return Ok(applied(false));
                    }
                    t.rows.push((row, expiry));
                    return Ok(applied(true));
                }
                match existing {
                    Some(i) => {
                        for (k, v) in row {
                            t.rows[i].0.insert(k, v);
                        }
                        if expiry.is_some() {
                            t.rows[i].1 = expiry;
                        }
                    }
                    None => t.rows.push((row, expiry)),
                }
                Ok(Outcome::Void)
            }
            Stmt::Update { table, sets, conds, if_exists, ttl } => {
                let (_, t) = self.table_mut(table, cur)?;
                for a in sets {
                    let c = match a {
                        Assign::Set(c, _) | Assign::Add(c, _) | Assign::Sub(c, _) | Assign::Prepend(c, _) | Assign::Index(c, _, _) => c,
                    };
                    if t.is_pk(c) {
                        return Err(invalid(format!("PRIMARY KEY part {} found in SET part", c)));
                    }
                }
                let keys = full_keys(t, conds, binds, true)?;
                let expiry = ttl_expiry(ttl, binds)?;
                let mut any = false;
                for key in keys {
                    t.purge();
                    let pos = t.rows.iter().position(|(r, _)| t_key(&t.partition, &t.clustering, r) == key);
                    let idx = match pos {
                        Some(i) => i,
                        None => {
                            if *if_exists {
                                continue;
                            }
                            let mut r = Row::new();
                            for (c, v) in t.partition.iter().chain(t.clustering.iter()).zip(key.iter()) {
                                r.insert(c.clone(), v.clone());
                            }
                            t.rows.push((r, None));
                            t.rows.len() - 1
                        }
                    };
                    any = true;
                    for a in sets {
                        apply_assign(t, idx, a, binds)?;
                    }
                    if expiry.is_some() {
                        t.rows[idx].1 = expiry;
                    }
                }
                if *if_exists {
                    return Ok(applied(any));
                }
                Ok(Outcome::Void)
            }
            Stmt::Delete { table, cols, conds, if_exists } => {
                let (_, t) = self.table_mut(table, cur)?;
                for k in &t.partition {
                    if !conds.iter().any(|c| &c.col == k && matches!(c.op, Op::Eq | Op::In)) {
                        return Err(invalid(format!("Some partition key parts are missing: {}", k)));
                    }
                }
                for c in cols {
                    t.col_type(c)?;
                }
                let mut matched = Vec::new();
                t.purge();
                for (i, (r, _)) in t.rows.iter().enumerate() {
                    if row_matches(r, conds, binds, &t.cols)? {
                        matched.push(i);
                    }
                }
                let any = !matched.is_empty();
                if cols.is_empty() {
                    for i in matched.into_iter().rev() {
                        t.rows.remove(i);
                    }
                } else {
                    for i in matched {
                        for c in cols {
                            t.rows[i].0.remove(c);
                        }
                    }
                }
                if *if_exists {
                    return Ok(applied(any));
                }
                Ok(Outcome::Void)
            }
            Stmt::Select { table, sel, conds, order, limit, allow_filtering, distinct } => {
                let (ks, t) = self.table_mut(table, cur)?;
                // restrictions
                for c in conds {
                    t.col_type(&c.col)?;
                }
                let pk_restricted = t.partition.iter().all(|k| conds.iter().any(|c| &c.col == k && matches!(c.op, Op::Eq | Op::In)));
                let only_keys = conds.iter().all(|c| t.is_pk(&c.col) && !matches!(c.op, Op::Ne | Op::Contains | Op::ContainsKey));
                let partition_only_eq = conds.iter().filter(|c| t.partition.contains(&c.col)).all(|c| matches!(c.op, Op::Eq | Op::In));
                if !conds.is_empty() && !*allow_filtering && !(pk_restricted && only_keys && partition_only_eq) {
                    return Err(invalid("Cannot execute this query as it might involve data filtering and thus may have unpredictable performance. If you want to execute this query despite the performance unpredictability, use ALLOW FILTERING"));
                }
                if let Some((oc, _)) = order {
                    if !t.clustering.contains(oc) {
                        return Err(invalid(format!("Order by is currently only supported on the clustered columns of the PRIMARY KEY, got {}", oc)));
                    }
                    if !pk_restricted {
                        return Err(invalid("ORDER BY is only supported when the partition key is restricted by an EQ or an IN."));
                    }
                }
                let cols = t.cols.clone();
                let mut rows: Vec<Row> = Vec::new();
                t.purge();
                for (r, _) in t.rows.iter() {
                    if row_matches(r, conds, binds, &cols)? {
                        rows.push(r.clone());
                    }
                }
                let (part, clus) = (t.partition.clone(), t.clustering.clone());
                rows.sort_by(|a, b| {
                    for c in part.iter().chain(clus.iter()) {
                        let o = cmp(a.get(c).unwrap_or(&Value::Null), b.get(c).unwrap_or(&Value::Null));
                        if o != Ordering::Equal {
                            return o;
                        }
                    }
                    Ordering::Equal
                });
                if let Some((oc, desc)) = order {
                    rows.sort_by(|a, b| {
                        let o = cmp(a.get(oc).unwrap_or(&Value::Null), b.get(oc).unwrap_or(&Value::Null));
                        if *desc {
                            o.reverse()
                        } else {
                            o
                        }
                    });
                }
                if *distinct {
                    let mut seen: Vec<Vec<Value>> = Vec::new();
                    rows.retain(|r| {
                        let k: Vec<Value> = part.iter().map(|c| r.get(c).cloned().unwrap_or(Value::Null)).collect();
                        if seen.contains(&k) {
                            false
                        } else {
                            seen.push(k);
                            true
                        }
                    });
                }
                let spec = select_columns(t, sel, &ks, &table.name)?;
                if let Selection::Count(_) = sel {
                    return Ok(Outcome::Rows { cols: spec, rows: vec![vec![Value::Int(rows.len() as i64)]] });
                }
                if let Some(l) = limit {
                    let n = eval(l, binds)?.as_i64().filter(|n| *n > 0).ok_or_else(|| invalid("LIMIT must be strictly positive"))?;
                    rows.truncate(n as usize);
                }
                let names: Vec<String> = match sel {
                    Selection::Cols(c) => c.iter().map(|(n, _)| n.clone()).collect(),
                    _ => spec.iter().map(|s| s.name.clone()).collect(),
                };
                let out = rows.iter().map(|r| names.iter().map(|n| r.get(n).cloned().unwrap_or(Value::Null)).collect()).collect();
                Ok(Outcome::Rows { cols: spec, rows: out })
            }
        }
    }
}

fn t_key(part: &[String], clus: &[String], r: &Row) -> Vec<Value> {
    part.iter().chain(clus.iter()).map(|c| r.get(c).cloned().unwrap_or(Value::Null)).collect()
}

fn applied(b: bool) -> Outcome {
    Outcome::Rows { cols: vec![ColumnSpec { keyspace: String::new(), table: String::new(), name: "[applied]".into(), cql_type: CqlType::Boolean }], rows: vec![vec![Value::Bool(b)]] }
}

fn ttl_expiry(ttl: &Option<Term>, binds: &[Value]) -> R<Option<Instant>> {
    match ttl {
        None => Ok(None),
        Some(t) => {
            let secs = eval(t, binds)?.as_i64().ok_or_else(|| invalid("Invalid TTL"))?;
            if secs < 0 {
                return Err(invalid("A TTL must be greater or equal to 0"));
            }
            Ok(if secs == 0 { None } else { Some(Instant::now() + Duration::from_secs(secs as u64)) })
        }
    }
}

fn cond_specs(specs: &mut [Option<ColumnSpec>], conds: &[Cond], t: &Table, ks: &str, tbl: &str) -> R<()> {
    for c in conds {
        let ty = t.col_type(&c.col)?.clone();
        let mk = |ty: CqlType| ColumnSpec { keyspace: ks.into(), table: tbl.into(), name: c.col.clone(), cql_type: ty };
        match (&c.op, &c.term) {
            (Op::In, Term::Bind(i)) => {
                if let Some(s) = specs.get_mut(*i) {
                    *s = Some(ColumnSpec { name: format!("in({})", c.col), ..mk(CqlType::List(Box::new(ty.clone()))) });
                }
            }
            (Op::In, Term::List(items)) => {
                for it in items {
                    if let Term::Bind(i) = it {
                        if let Some(s) = specs.get_mut(*i) {
                            *s = Some(mk(ty.clone()));
                        }
                    }
                }
            }
            (Op::Contains, Term::Bind(i)) => {
                let et = match &ty {
                    CqlType::List(e) | CqlType::Set(e) => (**e).clone(),
                    CqlType::Map(_, v) => (**v).clone(),
                    other => other.clone(),
                };
                if let Some(s) = specs.get_mut(*i) {
                    *s = Some(mk(et));
                }
            }
            (Op::ContainsKey, Term::Bind(i)) => {
                let kt = match &ty {
                    CqlType::Map(k, _) => (**k).clone(),
                    other => other.clone(),
                };
                if let Some(s) = specs.get_mut(*i) {
                    *s = Some(mk(kt));
                }
            }
            (_, Term::Bind(i)) => {
                if let Some(s) = specs.get_mut(*i) {
                    *s = Some(mk(ty.clone()));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn select_columns(t: &Table, sel: &Selection, ks: &str, tbl: &str) -> R<Vec<ColumnSpec>> {
    let mk = |n: &str, ty: CqlType| ColumnSpec { keyspace: ks.into(), table: tbl.into(), name: n.into(), cql_type: ty };
    Ok(match sel {
        Selection::All => {
            // partition keys, clustering keys, then the other columns by name (like Cassandra)
            let mut names: Vec<&String> = t.partition.iter().chain(t.clustering.iter()).collect();
            let mut rest: Vec<&String> = t.cols.iter().map(|(n, _)| n).filter(|n| !t.is_pk(n)).collect();
            rest.sort();
            names.extend(rest);
            names.into_iter().map(|n| Ok(mk(n, t.col_type(n)?.clone()))).collect::<R<_>>()?
        }
        Selection::Count(alias) => vec![mk(alias, CqlType::Bigint)],
        Selection::Cols(cols) => cols.iter().map(|(n, a)| Ok(mk(a.as_deref().unwrap_or(n), t.col_type(n)?.clone()))).collect::<R<_>>()?,
    })
}

fn row_matches(r: &Row, conds: &[Cond], binds: &[Value], cols: &[(String, CqlType)]) -> R<bool> {
    for c in conds {
        let ty = cols.iter().find(|(n, _)| *n == c.col).map(|(_, t)| t.clone()).ok_or_else(|| invalid(format!("Undefined column name {}", c.col)))?;
        let v = r.get(&c.col).cloned().unwrap_or(Value::Null);
        let raw = eval(&c.term, binds)?;
        let ok = match c.op {
            Op::In => {
                let items = raw.as_array().cloned().unwrap_or_else(|| vec![raw.clone()]);
                let mut hit = false;
                for it in items {
                    if cmp(&v, &normalize(&ty, &it)?) == Ordering::Equal {
                        hit = true;
                    }
                }
                hit
            }
            Op::Contains => {
                let et = match &ty {
                    CqlType::List(e) | CqlType::Set(e) => (**e).clone(),
                    CqlType::Map(_, x) => (**x).clone(),
                    other => other.clone(),
                };
                let want = normalize(&et, &raw)?;
                match &v {
                    Value::Array(a) => a.iter().any(|x| cmp(x, &want) == Ordering::Equal),
                    Value::Object(o) => o.values().any(|x| cmp(x, &want) == Ordering::Equal),
                    _ => false,
                }
            }
            Op::ContainsKey => v.as_object().is_some_and(|o| o.contains_key(&raw.to_string())),
            _ => {
                if v.is_null() {
                    false
                } else {
                    let o = cmp(&v, &normalize(&ty, &raw)?);
                    match c.op {
                        Op::Eq => o == Ordering::Equal,
                        Op::Ne => o != Ordering::Equal,
                        Op::Lt => o == Ordering::Less,
                        Op::Le => o != Ordering::Greater,
                        Op::Gt => o == Ordering::Greater,
                        _ => o != Ordering::Less,
                    }
                }
            }
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

/// All full primary keys designated by `=`/`IN` restrictions (cartesian product).
fn full_keys(t: &Table, conds: &[Cond], binds: &[Value], need_clustering: bool) -> R<Vec<Vec<Value>>> {
    let mut keys: Vec<Vec<Value>> = vec![vec![]];
    let cols: Vec<String> = t.partition.iter().chain(t.clustering.iter()).cloned().collect();
    for (i, k) in cols.iter().enumerate() {
        let is_part = i < t.partition.len();
        let c = conds.iter().find(|c| &c.col == k && matches!(c.op, Op::Eq | Op::In));
        let Some(c) = c else {
            if is_part {
                return Err(invalid(format!("Some partition key parts are missing: {}", k)));
            }
            if need_clustering {
                return Err(invalid(format!("Some clustering keys are missing: {}", k)));
            }
            break;
        };
        let ty = t.col_type(k)?.clone();
        let raw = eval(&c.term, binds)?;
        let vals: Vec<Value> = match c.op {
            Op::In => raw.as_array().cloned().unwrap_or_else(|| vec![raw.clone()]),
            _ => vec![raw],
        };
        let vals: Vec<Value> = vals.iter().map(|v| normalize(&ty, v)).collect::<R<_>>()?;
        keys = keys.into_iter().flat_map(|prefix| vals.iter().map(move |v| {
            let mut p = prefix.clone();
            p.push(v.clone());
            p
        })).collect();
    }
    for c in conds {
        if !t.is_pk(&c.col) {
            return Err(invalid(format!("Non PRIMARY KEY columns found in where clause: {}", c.col)));
        }
    }
    Ok(keys)
}

fn apply_assign(t: &mut Table, idx: usize, a: &Assign, binds: &[Value]) -> R<()> {
    let (col, ty) = match a {
        Assign::Set(c, _) | Assign::Add(c, _) | Assign::Sub(c, _) | Assign::Prepend(c, _) | Assign::Index(c, _, _) => (c.clone(), t.col_type(c)?.clone()),
    };
    let row = &mut t.rows[idx].0;
    let cur = row.get(&col).cloned().unwrap_or(Value::Null);
    let new = match a {
        Assign::Set(_, v) => {
            if matches!(ty, CqlType::Counter) {
                return Err(invalid(format!("Cannot set the value of counter column {} (counters can only be incremented/decremented, not set)", col)));
            }
            normalize(&ty, &eval(v, binds)?)?
        }
        Assign::Add(_, v) | Assign::Sub(_, v) => {
            let add = matches!(a, Assign::Add(..));
            let val = eval(v, binds)?;
            match &ty {
                CqlType::Counter => {
                    let d = val.as_i64().ok_or_else(|| invalid("Invalid counter increment"))?;
                    Value::Int(cur.as_i64().unwrap_or(0) + if add { d } else { -d })
                }
                CqlType::List(_) => {
                    let items = normalize(&ty, &val)?.as_array().cloned().unwrap_or_default();
                    let mut l = cur.as_array().cloned().unwrap_or_default();
                    if add {
                        l.extend(items);
                    } else {
                        l.retain(|x| !items.contains(x));
                    }
                    Value::Array(l)
                }
                CqlType::Set(_) => {
                    let items = normalize(&ty, &val)?.as_array().cloned().unwrap_or_default();
                    let mut l = cur.as_array().cloned().unwrap_or_default();
                    if add {
                        l.extend(items);
                    } else {
                        l.retain(|x| !items.contains(x));
                    }
                    normalize(&ty, &Value::Array(l))?
                }
                CqlType::Map(..) => {
                    let mut m = cur.as_object().cloned().unwrap_or_default();
                    if add {
                        for (k, v2) in normalize(&ty, &val)?.as_object().cloned().unwrap_or_default() {
                            m.insert(k, v2);
                        }
                    } else {
                        for k in val.as_array().cloned().unwrap_or_else(|| vec![val.clone()]) {
                            m.remove(&k.to_string());
                        }
                    }
                    Value::Object(m)
                }
                _ => return Err(invalid(format!("Invalid operation ({} = {} {} ?) for non counter column {}", col, col, if add { "+" } else { "-" }, col))),
            }
        }
        Assign::Prepend(_, v) => {
            let mut items = normalize(&ty, &eval(v, binds)?)?.as_array().cloned().unwrap_or_default();
            items.extend(cur.as_array().cloned().unwrap_or_default());
            Value::Array(items)
        }
        Assign::Index(_, k, v) => {
            let key = eval(k, binds)?;
            let val = eval(v, binds)?;
            match &ty {
                CqlType::Map(_, vt) => {
                    let mut m = cur.as_object().cloned().unwrap_or_default();
                    m.insert(key.to_string(), normalize(vt, &val)?);
                    Value::Object(m)
                }
                CqlType::List(et) => {
                    let mut l = cur.as_array().cloned().unwrap_or_default();
                    let i = key.as_i64().filter(|i| *i >= 0 && (*i as usize) < l.len()).ok_or_else(|| invalid("List index out of bound"))?;
                    l[i as usize] = normalize(et, &val)?;
                    Value::Array(l)
                }
                _ => return Err(invalid(format!("Invalid operation for non collection column {}", col))),
            }
        }
    };
    row.insert(col, new);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &mut Schema, q: &str, binds: &[Value], ks: Option<&str>) -> R<Outcome> {
        let (st, _) = parse(q)?;
        s.execute(&st, binds, ks)
    }

    fn rows(o: Outcome) -> Vec<Vec<Value>> {
        match o {
            Outcome::Rows { rows, .. } => rows,
            other => panic!("{:?}", other),
        }
    }

    #[test]
    fn lexer() {
        let t = lex("SELECT * FROM t WHERE id = 123e4567-e89b-12d3-a456-426614174000 AND b = 0xff AND s = 'it''s' AND n >= -1.5;").unwrap();
        assert!(t.contains(&Tok::Uuid("123e4567-e89b-12d3-a456-426614174000".into())));
        assert!(t.contains(&Tok::Blob(vec![0xff])));
        assert!(t.contains(&Tok::Str("it's".into())));
        assert!(t.contains(&Tok::Num("-1.5".into())));
        assert!(t.contains(&Tok::Sym(">=".into())));
        assert!(lex("SELECT 'x").is_err());
    }

    #[test]
    fn crud() {
        let mut s = Schema::default();
        run(&mut s, "CREATE KEYSPACE ks WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1}", &[], None).unwrap();
        assert_eq!(run(&mut s, "CREATE KEYSPACE ks WITH replication = {}", &[], None).unwrap_err().0, error_code::ALREADY_EXISTS);
        run(&mut s, "CREATE TABLE ks.users (id int PRIMARY KEY, name text, tags set<text>, visits counter)", &[], None).unwrap_err();
        run(&mut s, "CREATE TABLE ks.users (id int PRIMARY KEY, name text, tags set<text>, props map<text,int>)", &[], None).unwrap();
        run(&mut s, "CREATE TABLE ks.events (day text, ts int, msg text, PRIMARY KEY ((day), ts))", &[], None).unwrap();
        run(&mut s, "INSERT INTO users (id, name, tags) VALUES (1, 'ada', {'b','a'})", &[], Some("ks")).unwrap();
        run(&mut s, "INSERT INTO users (id, name) VALUES (?, ?)", &[Value::Int(2), Value::from("bob")], Some("ks")).unwrap();
        assert_eq!(rows(run(&mut s, "INSERT INTO ks.users (id, name) VALUES (2, 'x') IF NOT EXISTS", &[], None).unwrap()), vec![vec![Value::Bool(false)]]);
        let r = rows(run(&mut s, "SELECT * FROM ks.users WHERE id = 1", &[], None).unwrap());
        assert_eq!(r, vec![vec![Value::Int(1), Value::from("ada"), Value::Null, Value::Array(vec!["a".into(), "b".into()])]]);
        assert!(run(&mut s, "SELECT * FROM ks.users WHERE name = 'ada'", &[], None).unwrap_err().1.contains("ALLOW FILTERING"));
        assert_eq!(rows(run(&mut s, "SELECT id FROM ks.users WHERE name = 'bob' ALLOW FILTERING", &[], None).unwrap()), vec![vec![Value::Int(2)]]);
        run(&mut s, "UPDATE ks.users SET name = 'Ada', tags = tags + {'c'}, props['x'] = 5 WHERE id = 1", &[], None).unwrap();
        let r = rows(run(&mut s, "SELECT name, tags, props FROM ks.users WHERE id IN (1, 3)", &[], None).unwrap());
        assert_eq!(r, vec![vec![Value::from("Ada"), Value::Array(vec!["a".into(), "b".into(), "c".into()]), Value::object([("x", 5)])]]);
        assert!(run(&mut s, "UPDATE ks.users SET name = 'x' WHERE name = 'y'", &[], None).is_err());
        run(&mut s, "UPDATE ks.users SET name = 'new' WHERE id = 9", &[], None).unwrap();
        assert_eq!(rows(run(&mut s, "SELECT COUNT(*) FROM ks.users", &[], None).unwrap()), vec![vec![Value::Int(3)]]);
        run(&mut s, "DELETE FROM ks.users WHERE id = 9", &[], None).unwrap();
        run(&mut s, "DELETE name FROM ks.users WHERE id = 2", &[], None).unwrap();
        assert_eq!(rows(run(&mut s, "SELECT name FROM ks.users WHERE id = 2", &[], None).unwrap()), vec![vec![Value::Null]]);
        for i in [3, 1, 2] {
            run(&mut s, "INSERT INTO ks.events (day, ts, msg) VALUES ('mon', ?, 'm')", &[Value::Int(i)], None).unwrap();
        }
        let r = rows(run(&mut s, "SELECT ts FROM ks.events WHERE day = 'mon' AND ts > 1 ORDER BY ts DESC LIMIT 5", &[], None).unwrap());
        assert_eq!(r, vec![vec![Value::Int(3)], vec![Value::Int(2)]]);
        assert!(run(&mut s, "INSERT INTO ks.events (day, msg) VALUES ('x', 'y')", &[], None).unwrap_err().1.contains("clustering"));
        assert_eq!(run(&mut s, "SELEC x", &[], None).unwrap_err().0, error_code::SYNTAX_ERROR);
        assert!(run(&mut s, "SELECT * FROM ks.nope", &[], None).unwrap_err().1.contains("unconfigured table"));
        assert!(run(&mut s, "INSERT INTO ks.users (id, name) VALUES ('notint', 'x')", &[], None).is_err());
    }

    #[test]
    fn counters_ttl_and_prepare() {
        let mut s = Schema::default();
        run(&mut s, "CREATE KEYSPACE k WITH replication = {}", &[], None).unwrap();
        run(&mut s, "CREATE TABLE k.c (id text PRIMARY KEY, n counter)", &[], None).unwrap();
        run(&mut s, "UPDATE k.c SET n = n + 5 WHERE id = 'a'", &[], None).unwrap();
        run(&mut s, "UPDATE k.c SET n = n - 2 WHERE id = 'a'", &[], None).unwrap();
        assert_eq!(rows(run(&mut s, "SELECT n FROM k.c WHERE id = 'a'", &[], None).unwrap()), vec![vec![Value::Int(3)]]);
        run(&mut s, "CREATE TABLE k.t (id int PRIMARY KEY, v text, at timestamp)", &[], None).unwrap();
        run(&mut s, "INSERT INTO k.t (id, v) VALUES (1, 'x') USING TTL 1", &[], None).unwrap();
        assert_eq!(rows(run(&mut s, "SELECT COUNT(*) FROM k.t", &[], None).unwrap()), vec![vec![Value::Int(1)]]);
        let (st, n) = parse("SELECT v FROM k.t WHERE id IN ? LIMIT ?").unwrap();
        let (binds, result) = s.prepare(&st, n, None).unwrap();
        assert_eq!(binds[0].cql_type, CqlType::List(Box::new(CqlType::Int)));
        assert_eq!(binds[1].name, "[limit]");
        assert_eq!(result[0].name, "v");
        let (st, n) = parse("INSERT INTO k.t (id, v, at) VALUES (?, ?, ?)").unwrap();
        let (binds, _) = s.prepare(&st, n, None).unwrap();
        assert_eq!(binds.iter().map(|b| b.cql_type.clone()).collect::<Vec<_>>(), vec![CqlType::Int, CqlType::Varchar, CqlType::Timestamp]);
        s.execute(&st, &[Value::Int(5), Value::from("y"), Value::from("2024-01-01T00:00:00Z")], None).unwrap();
        assert_eq!(rows(run(&mut s, "SELECT at FROM k.t WHERE id = 5", &[], None).unwrap()), vec![vec![Value::Int(1_704_067_200_000)]]);
    }
}
