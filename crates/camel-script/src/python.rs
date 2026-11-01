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
    let mut p = P { toks, i: 0, saw_yield: false };
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
    let mut p = P { toks, i: 0, saw_yield: false };
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
                "class" => return Ok(vec![Stmt::Class(Arc::new(self.class_stmt(vec![])?))]),
                "with" => return Ok(vec![self.with_stmt()?]),
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
            return Ok(vec![Stmt::Class(Arc::new(self.class_stmt(decs)?))]);
        }
        if !self.is_kw("def") {
            return self.err(format!("invalid syntax: expected 'def' or 'class' after decorator but found {}", describe(self.peek())));
        }
        let pos = self.pos();
        let f = self.def_stmt()?;
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
                    // metaclass=... keyword
                    self.next();
                    self.next();
                    self.test()?;
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
        if dataclass {
            self.add_dataclass_methods(&name, &data_fields, &mut methods)?;
        }
        let mut bases = bases.into_iter();
        let extends = bases.next();
        Ok(ClassDef { name: Some(name), extends, ctors: vec![], methods, fields, mixins: bases.collect(), style: ClassStyle::Python, annotations: vec![] })
    }

    /// `@dataclass`: `__init__`, `__repr__` and `__eq__` from the annotated fields.
    fn add_dataclass_methods(&mut self, name: &str, data: &[(String, Option<Expr>)], methods: &mut Vec<ClassMember>) -> R<()> {
        let params: Vec<String> = data.iter().map(|(n, d)| if d.is_some() { format!("{}=None", n) } else { n.clone() }).collect();
        let assigns: String = data.iter().map(|(n, _)| format!("        self.{0} = {0}\n", n)).collect();
        let repr = data.iter().map(|(n, _)| format!("'{0}=' + repr(self.{0})", n)).collect::<Vec<_>>().join(" + ', ' + ");
        let mine = data.iter().map(|(n, _)| format!("self.{}", n)).collect::<Vec<_>>().join(", ");
        let theirs = data.iter().map(|(n, _)| format!("other.{}", n)).collect::<Vec<_>>().join(", ");
        let src = format!(
            "class {n}:\n    def __init__(self{c}{p}):\n{a}        pass\n    def __repr__(self):\n        return '{n}(' + {r} + ')'\n    def __eq__(self, other):\n        return isinstance(other, {n}) and [{m}] == [{t}]\n",
            n = name,
            c = if params.is_empty() { "" } else { ", " },
            p = params.join(", "),
            a = assigns,
            r = if repr.is_empty() { "''".to_string() } else { repr },
            m = mine,
            t = theirs
        );
        let prog = parse_program(&src)?;
        if let Some(Stmt::Class(c)) = prog.body.into_iter().next() {
            for m in &c.methods {
                if methods.iter().any(|x| x.name == m.name) {
                    continue;
                }
                let mut m = m.clone();
                if m.name == "__init__" {
                    // real default expressions
                    let mut f = (*m.func).clone();
                    for (p, (_, d)) in f.params.iter_mut().skip(1).zip(data.iter()) {
                        if let Some(d) = d {
                            p.default = Some(d.clone());
                        }
                    }
                    m.func = Arc::new(f);
                }
                methods.push(m);
            }
        }
        Ok(())
    }

    /// `with a as x, b as y: body`: `__enter__` / `__exit__` around the body.
    fn with_stmt(&mut self) -> R<Stmt> {
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
            let call = |obj: Expr, name: &str, args: Vec<Expr>| Expr::Call { callee: Box::new(Expr::Member { obj: Box::new(obj), name: name.into(), optional: false, pos }), args: args.into_iter().map(Elem::Item).collect(), optional: false, pos };
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
                    return Ok(if k == "global" { Stmt::Global(names) } else { Stmt::Empty });
                }
                "import" | "from" => {
                    while !self.is(&PT::Newline) && !self.is(&PT::Eof) && !self.is_op(";") {
                        self.next();
                    }
                    return Ok(Stmt::Empty);
                }
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
        while !self.is_op(close) {
            if self.eat_op("**") {
                let n = self.name()?;
                params.push(Param { pat: Pat::Id(n), default: Some(Expr::Object(vec![])) });
            } else if self.eat_op("*") {
                if let PT::Name(_) = self.peek() {
                    rest = Some(Pat::Id(self.name()?));
                }
            } else {
                let n = self.name()?;
                if close == ")" && self.eat_op(":") {
                    self.test()?;
                }
                let default = if self.eat_op("=") { Some(self.test()?) } else { None };
                params.push(Param { pat: Pat::Id(n), default });
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
        let base = self.atom_expr()?;
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
