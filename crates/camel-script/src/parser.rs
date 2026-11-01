//! Parser for the C-like languages: JavaScript, Groovy, MVEL (statements) and OGNL, SpEL
//! (single expressions). Produces the shared [`crate::ast`].

use std::sync::Arc;

use crate::ast::*;
use crate::error::SyntaxError;
use crate::lexer::{tokenize, RawPart, Tok, T};

type R<T> = Result<T, SyntaxError>;

/// Parse a whole program.
pub fn parse_program(src: &str, d: Dialect) -> R<Program> {
    let toks = tokenize(src, 0, d)?;
    let mut p = Parser { toks, i: 0, d, no_in: false, in_gen: false, in_async: d == Dialect::Js };
    let body = match d {
        Dialect::Ognl | Dialect::Spel => {
            let e = p.parse_sequence()?;
            p.expect_eof()?;
            vec![Stmt::Expr(e)]
        }
        _ => {
            let mut body = Vec::new();
            while !p.at_eof() {
                if p.eat_p(";") {
                    continue;
                }
                body.push(p.parse_statement()?);
            }
            body
        }
    };
    // JavaScript top-level await: run the script body as an async function
    if d == Dialect::Js && crate::gen::stmts_have_yield(&body) {
        let mut f = FuncDef::new(None, vec![], None, FuncBody::Block(body));
        f.arrow = true;
        f.is_async = true;
        f.implicit_return = true;
        let call = Expr::Call { callee: Box::new(Expr::Func(Arc::new(f))), args: vec![], optional: false, pos: 0 };
        return Ok(Program { dialect: d, body: vec![Stmt::Expr(call)] });
    }
    Ok(Program { dialect: d, body })
}

/// Parse an embedded expression (template interpolation) at absolute offset `base`.
pub fn parse_embedded(src: &str, base: usize, d: Dialect) -> R<Expr> {
    let toks = tokenize(src, base, d)?;
    let mut p = Parser { toks, i: 0, d, no_in: false, in_gen: false, in_async: false };
    if p.at_eof() {
        return Err(SyntaxError { pos: base, msg: "empty expression".into() });
    }
    let e = p.parse_expr()?;
    p.expect_eof()?;
    Ok(e)
}

struct Parser {
    toks: Vec<Tok>,
    i: usize,
    d: Dialect,
    no_in: bool,
    /// Inside a generator function (`yield` is an operator).
    in_gen: bool,
    /// Inside an async function or at the JavaScript top level (`await` is an operator).
    in_async: bool,
}

const JS_KEYWORDS: &[&str] = &[
    "var", "let", "const", "function", "return", "if", "else", "for", "while", "do", "break", "continue", "new", "delete", "typeof", "instanceof", "in", "of",
    "this", "null", "undefined", "true", "false", "throw", "try", "catch", "finally", "switch", "case", "default", "class", "void",
];

fn describe(t: &T) -> String {
    match t {
        T::Eof => "end of input".into(),
        T::Int(i) => format!("number {}", i),
        T::Num(f) => format!("number {}", f),
        T::Str(s) => format!("string '{}'", s),
        T::Tpl(_) => "template string".into(),
        T::Regex(s, _) => format!("regex /{}/", s),
        T::Ident(s) => format!("'{}'", s),
        T::P(p) => format!("'{}'", p),
    }
}

impl Parser {
    // ------------------------------------------------------------------ helpers
    fn peek(&self) -> &T {
        &self.toks[self.i].t
    }
    fn peek_at(&self, n: usize) -> &T {
        let j = (self.i + n).min(self.toks.len() - 1);
        &self.toks[j].t
    }
    fn tok_at(&self, n: usize) -> &Tok {
        let j = (self.i + n).min(self.toks.len() - 1);
        &self.toks[j]
    }
    fn pos(&self) -> u32 {
        self.toks[self.i].pos as u32
    }
    fn nl(&self) -> bool {
        self.toks[self.i].nl
    }
    fn next(&mut self) -> T {
        let t = self.toks[self.i].t.clone();
        if self.i < self.toks.len() - 1 {
            self.i += 1;
        }
        t
    }
    fn at_eof(&self) -> bool {
        matches!(self.peek(), T::Eof)
    }
    fn is_p(&self, p: &str) -> bool {
        matches!(self.peek(), T::P(x) if *x == p)
    }
    fn is_p_at(&self, n: usize, p: &str) -> bool {
        matches!(self.peek_at(n), T::P(x) if *x == p)
    }
    fn eat_p(&mut self, p: &str) -> bool {
        if self.is_p(p) {
            self.next();
            true
        } else {
            false
        }
    }
    fn is_kw(&self, k: &str) -> bool {
        matches!(self.peek(), T::Ident(x) if x == k)
    }
    fn is_kw_at(&self, n: usize, k: &str) -> bool {
        matches!(self.peek_at(n), T::Ident(x) if x == k)
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
    fn unexpected<X>(&self) -> R<X> {
        self.err(format!("unexpected {}", describe(self.peek())))
    }
    fn expect_p(&mut self, p: &str) -> R<()> {
        if self.eat_p(p) {
            Ok(())
        } else {
            self.err(format!("expected '{}' but found {}", p, describe(self.peek())))
        }
    }
    fn expect_eof(&self) -> R<()> {
        if self.at_eof() {
            Ok(())
        } else {
            self.err(format!("unexpected {} after end of expression", describe(self.peek())))
        }
    }
    fn ident(&mut self) -> R<String> {
        match self.peek().clone() {
            T::Ident(s) => {
                self.next();
                Ok(s)
            }
            _ => self.err(format!("expected identifier but found {}", describe(self.peek()))),
        }
    }
    /// Property name after `.`: any identifier or keyword (or a string in Groovy).
    fn prop_name(&mut self) -> R<String> {
        if self.d == Dialect::Js && self.is_p("#") {
            self.next();
            return Ok(format!("#{}", self.ident()?));
        }
        match self.peek().clone() {
            T::Ident(s) => {
                self.next();
                Ok(s)
            }
            T::Str(s) if self.d == Dialect::Groovy => {
                self.next();
                Ok(s)
            }
            T::P("@") if self.d == Dialect::Groovy && matches!(self.peek_at(1), T::Ident(_)) => {
                // direct field access `obj.@field`
                self.next();
                Ok(format!("@{}", self.ident()?))
            }
            _ => self.err(format!("expected property name but found {}", describe(self.peek()))),
        }
    }
    fn consume_semi(&mut self) -> R<()> {
        if self.eat_p(";") {
            return Ok(());
        }
        if self.nl() || self.at_eof() || self.is_p("}") {
            return Ok(());
        }
        self.err(format!("expected ';' or newline but found {}", describe(self.peek())))
    }
    fn groovy_like(&self) -> bool {
        matches!(self.d, Dialect::Groovy | Dialect::Mvel)
    }
    /// Groovy, MVEL or Java: typed declarations and `Type x : list` loops.
    fn typed_like(&self) -> bool {
        matches!(self.d, Dialect::Groovy | Dialect::Mvel | Dialect::Java)
    }

    // ------------------------------------------------------------------ statements
    fn parse_block_body(&mut self) -> R<Vec<Stmt>> {
        self.expect_p("{")?;
        let mut v = Vec::new();
        while !self.is_p("}") {
            if self.at_eof() {
                return self.err("expected '}' but found end of input");
            }
            if self.eat_p(";") {
                continue;
            }
            v.push(self.parse_statement()?);
        }
        self.next();
        Ok(v)
    }

    /// MVEL inline array `{1, 2, 3}` at the start of a statement.
    fn mvel_inline_array(&self) -> bool {
        let mut depth = 0;
        let mut n = 0;
        loop {
            match self.peek_at(n) {
                T::P("{") | T::P("(") | T::P("[") => depth += 1,
                T::P("}") | T::P(")") | T::P("]") => {
                    depth -= 1;
                    if depth == 0 {
                        return false;
                    }
                }
                T::P(",") if depth == 1 => return true,
                T::P(";") | T::Eof => return false,
                _ => {}
            }
            n += 1;
        }
    }

    fn parse_statement(&mut self) -> R<Stmt> {
        if self.is_p("{") && !(self.d == Dialect::Mvel && self.mvel_inline_array()) {
            return Ok(Stmt::Block(self.parse_block_body()?));
        }
        if self.eat_p(";") {
            return Ok(Stmt::Empty);
        }
        let d = self.d;
        if d == Dialect::Groovy && self.is_groovy_type_decl_start() {
            let annotations = self.parse_groovy_annotations()?;
            let c = self.parse_groovy_class(annotations)?;
            return Ok(Stmt::Class(Arc::new(c)));
        }
        if let T::Ident(w) = self.peek().clone() {
            // labels
            if self.is_p_at(1, ":") && matches!(d, Dialect::Js | Dialect::Groovy | Dialect::Java) && !JS_KEYWORDS.contains(&w.as_str()) && w != "default" {
                let after = self.peek_at(2).clone();
                if matches!(after, T::Ident(ref k) if k == "for" || k == "while" || k == "do") || self.tok_at(2).nl {
                    self.next();
                    self.next();
                    let s = self.parse_statement()?;
                    return Ok(Stmt::Labeled(w, Box::new(s)));
                }
            }
            match w.as_str() {
                "var" | "let" | "const" if d == Dialect::Js => {
                    let s = self.parse_var_decl()?;
                    self.consume_semi()?;
                    return Ok(s);
                }
                "function" if d == Dialect::Js && (matches!(self.peek_at(1), T::Ident(_)) || self.is_p_at(1, "*")) => {
                    self.next();
                    let f = self.parse_function_rest(false, false)?;
                    return Ok(Stmt::Func(Arc::new(f)));
                }
                "async" if d == Dialect::Js && self.is_kw_at(1, "function") && !self.tok_at(1).nl => {
                    self.next();
                    self.next();
                    let f = self.parse_function_rest(false, true)?;
                    return Ok(Stmt::Func(Arc::new(f)));
                }
                "class" if d == Dialect::Js => {
                    let c = self.parse_class()?;
                    return Ok(Stmt::Class(Arc::new(c)));
                }
                "if" => return self.parse_if(),
                "for" => return self.parse_for(),
                "foreach" if d == Dialect::Mvel => return self.parse_for(),
                "with" if d == Dialect::Mvel && self.is_p_at(1, "(") => return self.parse_mvel_with(),
                "while" => {
                    self.next();
                    self.expect_p("(")?;
                    let c = self.parse_expr()?;
                    self.expect_p(")")?;
                    let body = self.parse_statement()?;
                    return Ok(Stmt::While(c, Box::new(body), None));
                }
                "do" => {
                    self.next();
                    let body = self.parse_statement()?;
                    if !self.eat_kw("while") {
                        return self.err("expected 'while' after do body");
                    }
                    self.expect_p("(")?;
                    let c = self.parse_expr()?;
                    self.expect_p(")")?;
                    self.eat_p(";");
                    return Ok(Stmt::DoWhile(Box::new(body), c));
                }
                "return" => {
                    self.next();
                    if self.is_p(";") || self.is_p("}") || self.at_eof() || self.nl() {
                        self.eat_p(";");
                        return Ok(Stmt::Return(None));
                    }
                    let e = self.parse_expr()?;
                    self.consume_semi()?;
                    return Ok(Stmt::Return(Some(e)));
                }
                "break" | "continue" => {
                    self.next();
                    let label = match self.peek().clone() {
                        T::Ident(l) if !self.nl() && !JS_KEYWORDS.contains(&l.as_str()) => {
                            self.next();
                            Some(l)
                        }
                        _ => None,
                    };
                    self.consume_semi()?;
                    return Ok(if w == "break" { Stmt::Break(label) } else { Stmt::Continue(label) });
                }
                "throw" => {
                    let pos = self.pos();
                    self.next();
                    let e = self.parse_expr()?;
                    self.consume_semi()?;
                    return Ok(Stmt::Throw(e, pos));
                }
                "try" => return self.parse_try(),
                "switch" => return self.parse_switch(),
                "assert" if d == Dialect::Groovy => {
                    let pos = self.pos();
                    self.next();
                    let e = self.parse_expr()?;
                    let msg = if self.eat_p(":") || self.eat_p(",") { Some(self.parse_expr()?) } else { None };
                    self.consume_semi()?;
                    return Ok(Stmt::Assert(e, msg, pos));
                }
                "import" | "package" if d == Dialect::Groovy => {
                    let line_start = self.i;
                    self.next();
                    while !self.at_eof() && !(self.nl() && self.i > line_start) && !self.is_p(";") {
                        self.next();
                    }
                    self.eat_p(";");
                    return Ok(Stmt::Empty);
                }
                "yield" if d == Dialect::Java && !matches!(self.peek_at(1), T::P("=") | T::P("(") | T::P(".") | T::P(";")) => {
                    // value of a switch expression arm
                    self.next();
                    let e = self.parse_expr()?;
                    self.consume_semi()?;
                    return Ok(Stmt::SwitchYield(e));
                }
                "def" | "var" | "final" | "static" | "private" | "public" | "protected" if self.typed_like() && !(d == Dialect::Java && w == "def") => {
                    while matches!(self.peek(), T::Ident(m) if matches!(m.as_str(), "final" | "static" | "private" | "public" | "protected" | "synchronized")) {
                        self.next();
                    }
                    if self.is_kw("def") || self.is_kw("var") {
                        self.next();
                    }
                    if self.is_p("(") {
                        // def (a, b) = [1, 2]
                        self.next();
                        let mut items = Vec::new();
                        while !self.is_p(")") {
                            self.skip_groovy_type();
                            items.push(Some((Pat::Id(self.ident()?), None)));
                            if !self.eat_p(",") {
                                break;
                            }
                        }
                        self.expect_p(")")?;
                        self.expect_p("=")?;
                        let init = self.parse_expr()?;
                        self.consume_semi()?;
                        return Ok(Stmt::Var(VarKind::Let, vec![(Pat::Arr(items, None), Some(init))]));
                    }
                    // typed after modifiers: `final String x = ...`
                    if self.is_groovy_typed_decl() {
                        self.skip_groovy_type();
                    }
                    if matches!(self.peek(), T::Ident(_)) && self.is_p_at(1, "(") {
                        return self.parse_groovy_method();
                    }
                    return self.parse_groovy_decl_rest();
                }
                _ => {}
            }
            if self.typed_like() && self.is_groovy_typed_decl() {
                self.skip_groovy_type();
                if matches!(self.peek(), T::Ident(_)) && self.is_p_at(1, "(") {
                    return self.parse_groovy_method();
                }
                return self.parse_groovy_decl_rest();
            }
            if d == Dialect::Groovy && self.is_command_expression() {
                let pos = self.pos();
                let mut callee = Expr::Ident(self.ident()?, pos);
                while self.is_p(".") {
                    self.next();
                    let mpos = self.pos();
                    let name = self.prop_name()?;
                    callee = Expr::Member { obj: Box::new(callee), name, optional: false, pos: mpos };
                }
                let mut args = vec![Elem::Item(self.parse_expr()?)];
                while self.eat_p(",") {
                    args.push(Elem::Item(self.parse_expr()?));
                }
                self.consume_semi()?;
                return Ok(Stmt::Expr(Expr::Call { callee: Box::new(callee), args, optional: false, pos }));
            }
        }
        let e = self.parse_expr()?;
        self.consume_semi()?;
        Ok(Stmt::Expr(e))
    }

    // ------------------------------------------------------------------ Groovy classes
    /// At `@Annotation`, modifiers and `class` / `interface` / `trait` / `enum` + name.
    fn is_groovy_type_decl_start(&self) -> bool {
        let mut n = 0;
        loop {
            match self.peek_at(n) {
                T::P("@") if matches!(self.peek_at(n + 1), T::Ident(_)) => {
                    n += 2;
                    while self.is_p_at(n, ".") && matches!(self.peek_at(n + 1), T::Ident(_)) {
                        n += 2;
                    }
                    if self.is_p_at(n, "(") {
                        let mut depth = 0;
                        loop {
                            match self.peek_at(n) {
                                T::P("(") => depth += 1,
                                T::P(")") => depth -= 1,
                                T::Eof => return false,
                                _ => {}
                            }
                            n += 1;
                            if depth == 0 {
                                break;
                            }
                        }
                    }
                }
                T::Ident(w) if matches!(w.as_str(), "public" | "private" | "protected" | "abstract" | "final" | "static" | "sealed") => n += 1,
                T::Ident(w) if matches!(w.as_str(), "class" | "interface" | "trait" | "enum") => return matches!(self.peek_at(n + 1), T::Ident(_)),
                _ => return false,
            }
        }
    }

    /// `@Name`, `@a.b.Name(...)`: the simple names.
    fn parse_groovy_annotations(&mut self) -> R<Vec<String>> {
        let mut out = Vec::new();
        while self.is_p("@") && matches!(self.peek_at(1), T::Ident(_)) {
            self.next();
            let mut name = self.ident()?;
            while self.is_p(".") && matches!(self.peek_at(1), T::Ident(_)) {
                self.next();
                name = self.ident()?;
            }
            if self.is_p("(") {
                self.skip_balanced()?;
            }
            out.push(name);
        }
        Ok(out)
    }

    /// Skip a balanced `( ... )`, `< ... >` or `{ ... }` group starting at the current token.
    fn skip_balanced(&mut self) -> R<()> {
        let mut depth = 0i32;
        loop {
            match self.next() {
                T::P("(") | T::P("{") | T::P("[") | T::P("<") => depth += 1,
                T::P(")") | T::P("}") | T::P("]") | T::P(">") => depth -= 1,
                T::P(">>") => depth -= 2,
                T::Eof => return self.err("unbalanced brackets"),
                _ => {}
            }
            if depth <= 0 {
                return Ok(());
            }
        }
    }

    fn groovy_type_name(&mut self) -> R<String> {
        let mut name = self.ident()?;
        while self.is_p(".") && matches!(self.peek_at(1), T::Ident(_)) {
            self.next();
            name = self.ident()?;
        }
        if self.is_p("<") {
            self.skip_balanced()?;
        }
        Ok(name)
    }

    fn parse_groovy_class(&mut self, mut annotations: Vec<String>) -> R<ClassDef> {
        while matches!(self.peek(), T::Ident(w) if matches!(w.as_str(), "public" | "private" | "protected" | "abstract" | "final" | "static" | "sealed")) {
            self.next();
        }
        let kind = self.ident()?;
        let name = self.ident()?;
        if self.is_p("<") {
            self.skip_balanced()?;
        }
        let mut extends = None;
        let mut mixins = Vec::new();
        loop {
            if self.eat_kw("extends") {
                loop {
                    let pos = self.pos();
                    let t = self.groovy_type_name()?;
                    if kind == "interface" || kind == "trait" || extends.is_some() {
                        mixins.push(Expr::Ident(t, pos));
                    } else {
                        extends = Some(Expr::Ident(t, pos));
                    }
                    if !self.eat_p(",") {
                        break;
                    }
                }
            } else if self.eat_kw("implements") {
                loop {
                    let pos = self.pos();
                    mixins.push(Expr::Ident(self.groovy_type_name()?, pos));
                    if !self.eat_p(",") {
                        break;
                    }
                }
            } else if self.eat_kw("permits") {
                while !self.is_p("{") && !self.at_eof() {
                    self.next();
                }
            } else {
                break;
            }
        }
        self.expect_p("{")?;
        let mut ctors = Vec::new();
        let mut methods: Vec<ClassMember> = Vec::new();
        let mut fields = Vec::new();
        let mut constants: Vec<(String, Vec<Elem>, u32)> = Vec::new();
        if kind == "enum" {
            while let T::Ident(c) = self.peek().clone() {
                let pos = self.pos();
                self.next();
                let args = if self.is_p("(") { self.parse_args()? } else { vec![] };
                if self.is_p("{") {
                    self.skip_balanced()?;
                }
                constants.push((c, args, pos));
                if !self.eat_p(",") {
                    break;
                }
            }
            self.eat_p(";");
        }
        while !self.eat_p("}") {
            if self.at_eof() {
                return self.err("expected '}' but found end of input");
            }
            if self.eat_p(";") {
                continue;
            }
            self.parse_groovy_annotations()?;
            if self.is_groovy_type_decl_start() {
                let a = self.parse_groovy_annotations()?;
                let inner = self.parse_groovy_class(a)?;
                let n = inner.name.clone().unwrap_or_default();
                fields.push(ClassField { name: n, init: Some(Expr::Class(Arc::new(inner))), is_static: true });
                continue;
            }
            let mut is_static = false;
            let mut is_abstract = kind == "interface";
            while let T::Ident(m) = self.peek().clone() {
                match m.as_str() {
                    "static" => is_static = true,
                    "abstract" => is_abstract = true,
                    "default" => is_abstract = false,
                    "public" | "private" | "protected" | "final" | "synchronized" | "transient" | "volatile" | "native" | "strictfp" => {}
                    _ => break,
                }
                self.next();
            }
            if is_static && self.is_p("{") {
                let body = self.parse_block_body()?;
                let f = FuncDef::new(None, vec![], None, FuncBody::Block(body));
                let call = Expr::Call { callee: Box::new(Expr::Member { obj: Box::new(Expr::Func(Arc::new(f))), name: "call".into(), optional: false, pos: 0 }), args: vec![Elem::Item(Expr::This)], optional: false, pos: 0 };
                fields.push(ClassField { name: format!("%static{}", fields.len()), init: Some(call), is_static: true });
                continue;
            }
            if self.is_p("<") {
                self.skip_balanced()?;
            }
            let mut typed = false;
            if self.is_kw("def") || self.is_kw("var") {
                self.next();
                typed = true;
            } else if self.is_groovy_typed_decl() {
                self.skip_groovy_type();
                typed = true;
            }
            let mpos = self.pos();
            let mname = self.ident()?;
            if self.is_p("(") {
                let (params, rest) = self.parse_params()?;
                if self.eat_kw("throws") {
                    while !self.is_p("{") && !self.is_p(";") && !self.is_p("}") && !self.nl() && !self.at_eof() {
                        self.next();
                    }
                }
                if !self.is_p("{") {
                    // abstract / interface method declaration
                    self.consume_semi()?;
                    continue;
                }
                let _ = is_abstract;
                let body = self.parse_block_body()?;
                let f = FuncDef { name: Some(mname.clone()), params, rest, body: FuncBody::Block(body), arrow: false, implicit_it: false, implicit_return: true, generator: false, is_async: false };
                if !typed && mname == name {
                    ctors.push(Arc::new(f));
                } else {
                    methods.push(ClassMember { name: mname, key: None, func: Arc::new(f), kind: MemberKind::Method, is_static });
                }
                continue;
            }
            let _ = mpos;
            let mut fname = mname;
            loop {
                let init = if self.eat_p("=") { Some(self.parse_expr()?) } else { None };
                fields.push(ClassField { name: fname, init, is_static: is_static || kind == "interface" });
                if !self.eat_p(",") {
                    break;
                }
                self.skip_groovy_type();
                fname = self.ident()?;
            }
            self.consume_semi()?;
        }
        if kind == "enum" {
            let list = constants.iter().map(|(c, _, _)| format!("{}.{}", name, c)).collect::<Vec<_>>().join(", ");
            let src = format!(
                "class {n} {{\n def $enumName\n def $enumOrdinal\n static $mk(n, o, e) {{ e.$enumName = n; e.$enumOrdinal = o; e }}\n String name() {{ this.$enumName }}\n int ordinal() {{ this.$enumOrdinal }}\n String toString() {{ this.$enumName }}\n int compareTo(o) {{ this.$enumOrdinal <=> o.$enumOrdinal }}\n static values() {{ [{l}] }}\n static valueOf(String s) {{ def r = values().find {{ it.name() == s }}; if (r == null) throw new IllegalArgumentException(\"No enum constant {n}.\" + s); r }}\n}}",
                n = name,
                l = list
            );
            let prog = parse_program(&src, Dialect::Groovy)?;
            if let Some(Stmt::Class(g)) = prog.body.into_iter().next() {
                for m in &g.methods {
                    if !methods.iter().any(|x| x.name == m.name) {
                        methods.push(m.clone());
                    }
                }
                let mut consts = Vec::new();
                for (i, (c, args, pos)) in constants.into_iter().enumerate() {
                    let make = Expr::New { callee: Box::new(Expr::Ident(name.clone(), pos)), args, pos };
                    let init = Expr::Call {
                        callee: Box::new(Expr::Member { obj: Box::new(Expr::Ident(name.clone(), pos)), name: "$mk".into(), optional: false, pos }),
                        args: vec![Elem::Item(Expr::Str(c.clone())), Elem::Item(Expr::Int(i as i64)), Elem::Item(make)],
                        optional: false,
                        pos,
                    };
                    consts.push(ClassField { name: c, init: Some(init), is_static: true });
                }
                let mut all: Vec<ClassField> = g.fields.clone();
                all.extend(fields.iter().filter(|f| !f.is_static).cloned());
                consts.extend(fields.into_iter().filter(|f| f.is_static));
                all.extend(consts);
                fields = all;
            }
            annotations.push("%enum".into());
        }
        Ok(ClassDef { name: Some(name), extends, ctors, methods, fields, mixins, style: ClassStyle::Groovy, annotations })
    }

    /// `println "x"` / `log.info msg` (Groovy command expressions).
    fn is_command_expression(&self) -> bool {
        let mut n = 0;
        loop {
            if !matches!(self.peek_at(n), T::Ident(w) if !JS_KEYWORDS.contains(&w.as_str()) || n > 0) {
                return false;
            }
            if self.is_p_at(n + 1, ".") {
                n += 2;
                continue;
            }
            break;
        }
        let t = self.tok_at(n + 1);
        if t.nl {
            return false;
        }
        match &t.t {
            T::Str(_) | T::Tpl(_) | T::Int(_) | T::Num(_) => true,
            T::Ident(w) => !matches!(w.as_str(), "in" | "instanceof" | "as"),
            _ => false,
        }
    }

    fn is_groovy_typed_decl(&self) -> bool {
        let w = match self.peek() {
            T::Ident(w) => w.clone(),
            _ => return false,
        };
        let primitive = matches!(w.as_str(), "int" | "long" | "double" | "float" | "boolean" | "char" | "byte" | "short" | "void");
        let qualified = matches!(w.as_str(), "java" | "javax") && self.is_p_at(1, ".");
        if !primitive && !qualified && !w.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            return false;
        }
        // skip qualified name, generics and array brackets
        let mut n = 1;
        while self.is_p_at(n, ".") && matches!(self.peek_at(n + 1), T::Ident(_)) {
            n += 2;
        }
        if self.is_p_at(n, "<") {
            let mut depth = 0;
            loop {
                match self.peek_at(n) {
                    T::P("<") => depth += 1,
                    T::P(">") => depth -= 1,
                    T::P(">>") => depth -= 2,
                    T::Ident(_) | T::P(",") | T::P(".") | T::P("?") => {}
                    _ => return false,
                }
                n += 1;
                if depth <= 0 {
                    break;
                }
            }
        }
        while self.is_p_at(n, "[") && self.is_p_at(n + 1, "]") {
            n += 2;
        }
        if !matches!(self.peek_at(n), T::Ident(x) if !JS_KEYWORDS.contains(&x.as_str()) && x != "as") {
            return false;
        }
        let after = self.tok_at(n + 1);
        after.nl || matches!(after.t, T::P("=") | T::P(";") | T::P("(") | T::P(",") | T::Eof | T::P("}") | T::P(":") | T::P(")")) || matches!(&after.t, T::Ident(x) if x == "in")
    }

    fn skip_groovy_type(&mut self) {
        if !self.is_groovy_typed_decl() {
            return;
        }
        self.next();
        while self.is_p(".") && matches!(self.peek_at(1), T::Ident(_)) {
            self.next();
            self.next();
        }
        if self.is_p("<") {
            let mut depth = 0;
            loop {
                match self.next() {
                    T::P("<") => depth += 1,
                    T::P(">") => depth -= 1,
                    T::P(">>") => depth -= 2,
                    T::Eof => break,
                    _ => {}
                }
                if depth <= 0 {
                    break;
                }
            }
        }
        while self.is_p("[") && self.is_p_at(1, "]") {
            self.next();
            self.next();
        }
    }

    fn parse_groovy_decl_rest(&mut self) -> R<Stmt> {
        let mut decls = Vec::new();
        loop {
            let name = self.ident()?;
            let init = if self.eat_p("=") { Some(self.parse_expr()?) } else { None };
            decls.push((Pat::Id(name), init));
            if !self.eat_p(",") {
                break;
            }
            self.skip_groovy_type();
        }
        self.consume_semi()?;
        Ok(Stmt::Var(VarKind::Let, decls))
    }

    fn parse_groovy_method(&mut self) -> R<Stmt> {
        let name = self.ident()?;
        let (params, rest) = self.parse_params()?;
        if self.is_kw("throws") {
            while !self.is_p("{") && !self.at_eof() {
                self.next();
            }
        }
        let body = self.parse_block_body()?;
        Ok(Stmt::Func(Arc::new(FuncDef { name: Some(name), params, rest, body: FuncBody::Block(body), arrow: false, implicit_it: false, implicit_return: true, generator: false, is_async: false })))
    }

    fn parse_var_decl(&mut self) -> R<Stmt> {
        let kind = match self.next() {
            T::Ident(w) if w == "let" => VarKind::Let,
            T::Ident(w) if w == "const" => VarKind::Const,
            _ => VarKind::Var,
        };
        let mut decls = Vec::new();
        loop {
            let pat = self.parse_binding_pattern()?;
            let init = if self.eat_p("=") { Some(self.parse_assign()?) } else { None };
            decls.push((pat, init));
            if !self.eat_p(",") {
                break;
            }
        }
        Ok(Stmt::Var(kind, decls))
    }

    fn parse_binding_pattern(&mut self) -> R<Pat> {
        if self.is_p("[") || self.is_p("{") {
            let save = self.no_in;
            self.no_in = false;
            let e = self.parse_primary()?;
            self.no_in = save;
            return self.to_pat(e);
        }
        if self.typed_like() {
            if self.is_kw("final") {
                self.next();
            }
            self.skip_groovy_type();
        }
        Ok(Pat::Id(self.ident()?))
    }

    fn parse_if(&mut self) -> R<Stmt> {
        self.next();
        self.expect_p("(")?;
        let c = self.parse_expr()?;
        self.expect_p(")")?;
        let then = self.parse_statement()?;
        let save = self.i;
        self.eat_p(";");
        let els = if self.eat_kw("else") {
            Some(Box::new(self.parse_statement()?))
        } else {
            self.i = save;
            None
        };
        Ok(Stmt::If(c, Box::new(then), els))
    }

    fn parse_for(&mut self) -> R<Stmt> {
        let foreach = self.is_kw("foreach");
        self.next();
        if self.d == Dialect::Js && matches!(self.peek(), T::Ident(w) if w == "await") {
            // `for await (x of xs)`: each element is awaited
            let pos = self.pos();
            self.next();
            let st = self.parse_for_rest(foreach)?;
            return match st {
                Stmt::ForIn { decl, pat, obj, body, values: true, else_body } => {
                    let tmp = "%forawait".to_string();
                    let bind = match decl {
                        Some(k) => Stmt::Var(k, vec![(pat, Some(Expr::Await(Box::new(Expr::Ident(tmp.clone(), pos)), pos)))]),
                        None => Stmt::Expr(Expr::Assign { op: None, target: Box::new(pat), value: Box::new(Expr::Await(Box::new(Expr::Ident(tmp.clone(), pos)), pos)), pos }),
                    };
                    Ok(Stmt::ForIn { decl: Some(VarKind::Let), pat: Pat::Id(tmp), obj, body: Box::new(Stmt::Block(vec![bind, *body])), values: true, else_body })
                }
                _ => self.err("expected 'for await (x of iterable)'"),
            };
        }
        self.parse_for_rest(foreach)
    }

    fn parse_for_rest(&mut self, foreach: bool) -> R<Stmt> {
        self.expect_p("(")?;
        // for-each forms of Groovy / MVEL / Java: `x in list`, `Type x : list`, `def x in list`
        if self.typed_like() {
            let mut n = 0;
            if self.is_kw("def") || self.is_kw("var") || self.is_kw("final") {
                n = 1;
            }
            if self.is_kw("final") && self.is_kw_at(1, "var") {
                n = 2;
            }
            let typed = self.groovy_type_len(n);
            let nm = n + typed;
            if matches!(self.peek_at(nm), T::Ident(_)) && (self.is_kw_at(nm + 1, "in") || self.is_p_at(nm + 1, ":")) {
                for _ in 0..nm {
                    self.next();
                }
                let name = self.ident()?;
                self.next();
                let obj = self.parse_expr()?;
                self.expect_p(")")?;
                let body = self.parse_statement()?;
                return Ok(Stmt::ForIn { decl: Some(VarKind::Let), pat: Pat::Id(name), obj, body: Box::new(body), values: true, else_body: None });
            }
            if foreach {
                return self.err("expected 'name : collection' in foreach");
            }
        }
        let init: Option<Box<Stmt>> = if self.is_p(";") {
            None
        } else {
            self.no_in = true;
            let s = if self.d == Dialect::Js && (self.is_kw("var") || self.is_kw("let") || self.is_kw("const")) {
                self.parse_var_decl()
            } else if self.typed_like() && (self.is_kw("def") || self.is_kw("var") || self.is_groovy_typed_decl()) {
                if self.is_kw("def") || self.is_kw("var") {
                    self.next();
                }
                self.skip_groovy_type();
                let mut decls = Vec::new();
                loop {
                    let name = self.ident()?;
                    let init = if self.eat_p("=") { Some(self.parse_assign()?) } else { None };
                    decls.push((Pat::Id(name), init));
                    if !self.eat_p(",") {
                        break;
                    }
                }
                Ok(Stmt::Var(VarKind::Let, decls))
            } else {
                self.parse_expr().map(Stmt::Expr)
            };
            self.no_in = false;
            Some(Box::new(s?))
        };
        if self.is_kw("in") || self.is_kw("of") {
            let values = self.is_kw("of") || self.d != Dialect::Js;
            self.next();
            let (decl, pat) = match init.map(|b| *b) {
                Some(Stmt::Var(k, mut decls)) if decls.len() == 1 && decls[0].1.is_none() => (Some(k), decls.remove(0).0),
                Some(Stmt::Expr(e)) => (None, self.to_pat(e)?),
                _ => return self.err("invalid for-in left side"),
            };
            let obj = self.parse_expr()?;
            self.expect_p(")")?;
            let body = self.parse_statement()?;
            return Ok(Stmt::ForIn { decl, pat, obj, body: Box::new(body), values, else_body: None });
        }
        self.expect_p(";")?;
        let test = if self.is_p(";") { None } else { Some(self.parse_expr()?) };
        self.expect_p(";")?;
        let update = if self.is_p(")") { None } else { Some(self.parse_expr()?) };
        self.expect_p(")")?;
        let body = self.parse_statement()?;
        Ok(Stmt::For { init, test, update, body: Box::new(body) })
    }

    /// Number of tokens of a Groovy type name at offset `n` (0 if none).
    fn groovy_type_len(&self, n: usize) -> usize {
        let w = match self.peek_at(n) {
            T::Ident(w) => w.clone(),
            _ => return 0,
        };
        let primitive = matches!(w.as_str(), "int" | "long" | "double" | "float" | "boolean" | "char" | "byte" | "short");
        if !primitive && !w.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            return 0;
        }
        let mut m = n + 1;
        while self.is_p_at(m, ".") && matches!(self.peek_at(m + 1), T::Ident(_)) {
            m += 2;
        }
        if self.is_p_at(m, "<") {
            let mut depth = 0;
            loop {
                match self.peek_at(m) {
                    T::P("<") => depth += 1,
                    T::P(">") => depth -= 1,
                    T::P(">>") => depth -= 2,
                    T::Ident(_) | T::P(",") | T::P(".") | T::P("?") => {}
                    _ => return 0,
                }
                m += 1;
                if depth <= 0 {
                    break;
                }
            }
        }
        while self.is_p_at(m, "[") && self.is_p_at(m + 1, "]") {
            m += 2;
        }
        if !matches!(self.peek_at(m), T::Ident(_)) {
            return 0;
        }
        m - n
    }

    fn parse_try(&mut self) -> R<Stmt> {
        self.next();
        let block = self.parse_block_body()?;
        let mut catches = Vec::new();
        while self.eat_kw("catch") {
            let mut types = Vec::new();
            let mut param = None;
            if self.eat_p("(") {
                // Groovy/Java: `Type1 | Type2 name`, `name`; JS: pattern
                if self.d == Dialect::Js {
                    param = Some(self.parse_binding_pattern()?);
                } else {
                    let mut names = vec![self.qualified_name()?];
                    while self.eat_p("|") {
                        names.push(self.qualified_name()?);
                    }
                    if let T::Ident(n) = self.peek().clone() {
                        self.next();
                        types = names;
                        param = Some(Pat::Id(n));
                    } else {
                        param = Some(Pat::Id(names.pop().unwrap()));
                        types = names;
                    }
                }
                self.expect_p(")")?;
            }
            let body = self.parse_block_body()?;
            let types = types.into_iter().map(|t| t.rsplit('.').next().unwrap_or(&t).to_string()).collect();
            catches.push(Catch { types, param, body });
        }
        let finally = if self.eat_kw("finally") { Some(self.parse_block_body()?) } else { None };
        if catches.is_empty() && finally.is_none() {
            return self.err("expected 'catch' or 'finally' after try block");
        }
        Ok(Stmt::Try { block, catches, finally })
    }

    fn qualified_name(&mut self) -> R<String> {
        let mut s = self.ident()?;
        while self.is_p(".") && matches!(self.peek_at(1), T::Ident(_)) {
            self.next();
            s.push('.');
            s.push_str(&self.ident()?);
        }
        Ok(s)
    }

    fn parse_switch(&mut self) -> R<Stmt> {
        if self.d == Dialect::Java && self.switch_has_arrows() {
            let e = self.parse_java_switch(true)?;
            self.eat_p(";");
            return Ok(Stmt::Expr(e));
        }
        self.next();
        self.expect_p("(")?;
        let disc = self.parse_expr()?;
        self.expect_p(")")?;
        self.expect_p("{")?;
        let mut cases: Vec<(Option<Expr>, Vec<Stmt>)> = Vec::new();
        while !self.eat_p("}") {
            let test = if self.eat_kw("case") {
                let mut e = self.parse_cond()?;
                if self.d == Dialect::Java && self.is_p(",") {
                    // `case 1, 2:`
                    let mut items = vec![Elem::Item(e)];
                    while self.eat_p(",") {
                        items.push(Elem::Item(self.parse_cond()?));
                    }
                    e = Expr::Array(items);
                }
                Some(e)
            } else if self.eat_kw("default") {
                None
            } else {
                return self.err(format!("expected 'case' or 'default' but found {}", describe(self.peek())));
            };
            if !self.eat_p(":") && !self.eat_p("->") {
                return self.err(format!("expected ':' but found {}", describe(self.peek())));
            }
            let mut body = Vec::new();
            while !self.is_kw("case") && !self.is_kw("default") && !self.is_p("}") {
                if self.at_eof() {
                    return self.err("unterminated switch");
                }
                if self.eat_p(";") {
                    continue;
                }
                body.push(self.parse_statement()?);
            }
            cases.push((test, body));
        }
        Ok(Stmt::Switch(disc, cases))
    }

    /// MVEL `with (obj) { prop = value, other = value }`: assigns properties of `obj`; the
    /// statement's value is the object.
    fn parse_mvel_with(&mut self) -> R<Stmt> {
        let pos = self.pos();
        self.next();
        self.expect_p("(")?;
        let target = self.parse_expr()?;
        self.expect_p(")")?;
        self.expect_p("{")?;
        let tmp = format!("%with{}", pos);
        let id = || Expr::Ident(tmp.clone(), pos);
        let mut stmts = vec![Stmt::Var(VarKind::Let, vec![(Pat::Id(tmp.clone()), Some(target))])];
        while !self.eat_p("}") {
            if self.at_eof() {
                return self.err("unterminated with block");
            }
            if self.eat_p(",") || self.eat_p(";") {
                continue;
            }
            let mpos = self.pos();
            let name = self.ident()?;
            let op = if self.eat_p("=") {
                None
            } else if self.eat_p("+=") {
                Some(AssignOp::Bin(BinOp::Add))
            } else if self.eat_p("-=") {
                Some(AssignOp::Bin(BinOp::Sub))
            } else {
                return self.err(format!("expected '=' in with block but found {}", describe(self.peek())));
            };
            let value = self.parse_assign()?;
            let target = Pat::Target(Expr::Member { obj: Box::new(id()), name, optional: false, pos: mpos });
            stmts.push(Stmt::Expr(Expr::Assign { op, target: Box::new(target), value: Box::new(value), pos: mpos }));
        }
        stmts.push(Stmt::Expr(id()));
        Ok(Stmt::Block(stmts))
    }

    // ------------------------------------------------------------------ Java
    /// At `switch (...) {`: whether the first case uses `->`.
    fn switch_has_arrows(&self) -> bool {
        let mut n = 0;
        let mut depth = 0;
        loop {
            match self.peek_at(n) {
                T::P("(") | T::P("[") => depth += 1,
                T::P(")") | T::P("]") => depth -= 1,
                T::P("{") => {
                    depth += 1;
                }
                T::P("}") => {
                    depth -= 1;
                    if depth <= 0 {
                        return false;
                    }
                }
                T::P("->") if depth == 1 => return true,
                T::P(":") if depth == 1 => return false,
                T::Eof => return false,
                _ => {}
            }
            n += 1;
        }
    }

    /// A Java switch expression (or an arrow-form switch statement): `switch (x) { case A, B -> e;
    /// case C -> { ...; yield v; } case String s -> ...; default -> e; }` or the colon form with
    /// `yield`.
    fn parse_java_switch(&mut self, statement: bool) -> R<Expr> {
        let pos = self.pos();
        self.next();
        self.expect_p("(")?;
        let disc = self.parse_expr()?;
        self.expect_p(")")?;
        self.expect_p("{")?;
        let mut arms: Vec<(Vec<Expr>, Vec<Stmt>)> = Vec::new();
        let mut has_default = false;
        while !self.eat_p("}") {
            if self.at_eof() {
                return self.err("unterminated switch");
            }
            let mut labels = Vec::new();
            if self.eat_kw("default") {
                has_default = true;
            } else if self.eat_kw("case") {
                loop {
                    if self.is_kw("default") {
                        self.next();
                        has_default = true;
                        labels.clear();
                        break;
                    }
                    // type pattern `case String s`
                    let tl = self.groovy_type_len(0);
                    if tl > 0 && matches!(self.peek_at(tl), T::Ident(_)) && matches!(self.peek_at(tl + 1), T::P("->") | T::P(":") | T::P(",")) {
                        let lpos = self.pos();
                        let ty = self.type_name()?;
                        let name = self.ident()?;
                        labels.push(Expr::InstanceBind { expr: Box::new(Expr::Undef), ty, name, pos: lpos });
                    } else {
                        labels.push(self.parse_cond()?);
                    }
                    if !self.eat_p(",") {
                        break;
                    }
                }
            } else {
                return self.err(format!("expected 'case' or 'default' but found {}", describe(self.peek())));
            }
            let body = if self.eat_p("->") {
                if self.is_p("{") {
                    self.parse_block_body()?
                } else if self.is_kw("throw") {
                    vec![self.parse_statement()?]
                } else {
                    let e = self.parse_expr()?;
                    self.expect_p(";")?;
                    vec![Stmt::Expr(e)]
                }
            } else {
                self.expect_p(":")?;
                let mut body = Vec::new();
                while !self.is_kw("case") && !self.is_kw("default") && !self.is_p("}") {
                    if self.at_eof() {
                        return self.err("unterminated switch");
                    }
                    if self.eat_p(";") {
                        continue;
                    }
                    body.push(self.parse_statement()?);
                }
                body
            };
            if labels.is_empty() {
                // default: keep it last
                arms.push((vec![], body));
            } else {
                arms.push((labels, body));
            }
        }
        // default arm evaluated last
        if let Some(i) = arms.iter().position(|(l, _)| l.is_empty()) {
            let d = arms.remove(i);
            arms.push(d);
        }
        if statement && !has_default {
            arms.push((vec![], vec![]));
        }
        Ok(Expr::SwitchExpr(Box::new(disc), arms, pos))
    }

    /// At `(`: whether this is a cast `(Type) operand`.
    fn is_java_cast(&self) -> bool {
        let w = match self.peek_at(1) {
            T::Ident(w) => w.clone(),
            _ => return false,
        };
        let primitive = matches!(w.as_str(), "int" | "long" | "double" | "float" | "boolean" | "char" | "byte" | "short");
        let tl = self.groovy_type_len_any(1);
        if tl == 0 || !self.is_p_at(1 + tl, ")") {
            return false;
        }
        if primitive {
            return true;
        }
        if !w.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            return false;
        }
        match self.peek_at(2 + tl) {
            T::Ident(x) => !matches!(x.as_str(), "instanceof"),
            T::Int(_) | T::Num(_) | T::Str(_) | T::Tpl(_) => true,
            T::P("(") | T::P("!") | T::P("~") => true,
            _ => false,
        }
    }

    /// Length in tokens of a type at offset `n` (qualified name, generics, array dims); 0 if none.
    fn groovy_type_len_any(&self, n: usize) -> usize {
        if !matches!(self.peek_at(n), T::Ident(_)) {
            return 0;
        }
        let mut m = n + 1;
        while self.is_p_at(m, ".") && matches!(self.peek_at(m + 1), T::Ident(_)) {
            m += 2;
        }
        if self.is_p_at(m, "<") {
            let mut depth = 0;
            loop {
                match self.peek_at(m) {
                    T::P("<") => depth += 1,
                    T::P(">") => depth -= 1,
                    T::P(">>") => depth -= 2,
                    T::Ident(_) | T::P(",") | T::P(".") | T::P("?") => {}
                    _ => return 0,
                }
                m += 1;
                if depth <= 0 {
                    break;
                }
            }
        }
        while self.is_p_at(m, "[") && self.is_p_at(m + 1, "]") {
            m += 2;
        }
        m - n
    }

    /// At `(`: whether a Java lambda `(a, b) -> ...` / `(Type a) -> ...` / `() -> ...` starts here.
    fn is_java_lambda_paren(&self) -> bool {
        let mut depth = 0;
        let mut n = 0;
        loop {
            match self.peek_at(n) {
                T::P("(") => depth += 1,
                T::P(")") => {
                    depth -= 1;
                    if depth == 0 {
                        return self.is_p_at(n + 1, "->");
                    }
                }
                T::Ident(_) | T::P(",") | T::P("<") | T::P(">") | T::P(".") | T::P("[") | T::P("]") | T::P("?") => {}
                _ => return false,
            }
            n += 1;
        }
    }

    fn parse_java_lambda(&mut self) -> R<Expr> {
        let mut params = Vec::new();
        if self.eat_p("(") {
            while !self.is_p(")") {
                if self.is_kw("final") {
                    self.next();
                }
                if self.is_kw("var") {
                    self.next();
                }
                let tl = self.groovy_type_len_any(0);
                if tl > 0 && matches!(self.peek_at(tl), T::Ident(_)) {
                    for _ in 0..tl {
                        self.next();
                    }
                }
                params.push(Param { pat: Pat::Id(self.ident()?), default: None });
                if !self.eat_p(",") {
                    break;
                }
            }
            self.expect_p(")")?;
        } else {
            params.push(Param { pat: Pat::Id(self.ident()?), default: None });
        }
        self.expect_p("->")?;
        let body = if self.is_p("{") { FuncBody::Block(self.parse_block_body()?) } else { FuncBody::Expr(Box::new(self.parse_assign()?)) };
        Ok(Expr::Func(Arc::new(FuncDef { name: None, params, rest: None, body, arrow: true, implicit_it: false, implicit_return: false, generator: false, is_async: false })))
    }

    fn parse_class(&mut self) -> R<ClassDef> {
        self.next();
        let name = if let T::Ident(n) = self.peek().clone() {
            if n != "extends" {
                self.next();
                Some(n)
            } else {
                None
            }
        } else {
            None
        };
        let extends = if self.eat_kw("extends") { Some(self.parse_postfix()?) } else { None };
        self.expect_p("{")?;
        let mut ctors = Vec::new();
        let mut methods = Vec::new();
        let mut fields = Vec::new();
        while !self.eat_p("}") {
            if self.eat_p(";") {
                continue;
            }
            let is_static = self.is_kw("static") && !self.is_p_at(1, "(") && !self.is_p_at(1, "=");
            if is_static {
                self.next();
                if self.is_p("{") {
                    // static initialization block
                    let body = self.parse_block_body()?;
                    let f = FuncDef::new(None, vec![], None, FuncBody::Block(body));
                    let call = Expr::Call { callee: Box::new(Expr::Member { obj: Box::new(Expr::Func(Arc::new(f))), name: "call".into(), optional: false, pos: 0 }), args: vec![Elem::Item(Expr::This)], optional: false, pos: 0 };
                    fields.push(ClassField { name: format!("%static{}", fields.len()), init: Some(call), is_static: true });
                    continue;
                }
            }
            let (m, key) = self.parse_method_head()?;
            if m.accessor.is_none() && !m.generator && !m.is_async && (self.is_p("=") || self.is_p(";") || self.is_p("}") || self.nl()) && !self.is_p("(") {
                // class field
                let init = if self.eat_p("=") { Some(self.parse_assign()?) } else { None };
                self.consume_semi()?;
                let PropKey::Name(n) = key else { return self.err("computed class field names are not supported") };
                fields.push(ClassField { name: n, init, is_static });
                continue;
            }
            let mut f = self.parse_method_body(m.generator, m.is_async)?;
            let (mname, mkey) = match key {
                PropKey::Name(n) => (n, None),
                PropKey::Computed(e) => (String::new(), Some(e)),
            };
            f.name = Some(mname.clone());
            if mname == "constructor" && !is_static && m.accessor.is_none() {
                ctors.push(Arc::new(f));
            } else {
                let kind = match m.accessor {
                    Some(true) => MemberKind::Getter,
                    Some(false) => MemberKind::Setter,
                    None => MemberKind::Method,
                };
                methods.push(ClassMember { name: mname, key: mkey, func: Arc::new(f), kind, is_static });
            }
        }
        Ok(ClassDef { name, extends, ctors, methods, fields, mixins: vec![], style: ClassStyle::Js, annotations: vec![] })
    }

    /// Method head in classes / object literals: modifiers (`get`, `set`, `async`, `*`) and the
    /// member name.
    fn parse_method_head(&mut self) -> R<(MethodHead, PropKey)> {
        let mut h = MethodHead { accessor: None, generator: false, is_async: false };
        let is_name_next = |p: &Parser, n: usize| matches!(p.peek_at(n), T::Ident(_) | T::Str(_) | T::Int(_) | T::Num(_) | T::P("[") | T::P("#") | T::P("*"));
        if (self.is_kw("get") || self.is_kw("set")) && is_name_next(self, 1) && !self.is_p_at(1, "*") {
            h.accessor = Some(self.is_kw("get"));
            self.next();
        } else if self.is_kw("async") && is_name_next(self, 1) && !self.tok_at(1).nl {
            h.is_async = true;
            self.next();
        }
        if self.eat_p("*") {
            h.generator = true;
        }
        let key = match self.peek().clone() {
            T::Ident(s) => {
                self.next();
                PropKey::Name(s)
            }
            T::Str(s) => {
                self.next();
                PropKey::Name(s)
            }
            T::Int(i) => {
                self.next();
                PropKey::Name(i.to_string())
            }
            T::Num(f) => {
                self.next();
                PropKey::Name(crate::interp::js_num_str(f))
            }
            T::P("#") => {
                self.next();
                PropKey::Name(format!("#{}", self.ident()?))
            }
            T::P("[") => {
                self.next();
                let k = self.parse_assign()?;
                self.expect_p("]")?;
                PropKey::Computed(k)
            }
            _ => return self.err(format!("unexpected {} in class body", describe(self.peek()))),
        };
        Ok((h, key))
    }

    fn parse_method_body(&mut self, generator: bool, is_async: bool) -> R<FuncDef> {
        let (sg, sa) = (self.in_gen, self.in_async);
        self.in_gen = generator;
        self.in_async = is_async;
        let r = self.parse_function_params_body();
        self.in_gen = sg;
        self.in_async = sa;
        let mut f = r?;
        f.generator = generator;
        f.is_async = is_async;
        Ok(f)
    }

    // ------------------------------------------------------------------ functions
    fn parse_params(&mut self) -> R<(Vec<Param>, Option<Pat>)> {
        self.expect_p("(")?;
        let mut params = Vec::new();
        let mut rest = None;
        while !self.is_p(")") {
            if self.eat_p("...") {
                rest = Some(self.parse_binding_pattern()?);
                break;
            }
            let pat = self.parse_binding_pattern()?;
            let default = if self.eat_p("=") { Some(self.parse_assign()?) } else { None };
            params.push(Param { pat, default });
            if !self.eat_p(",") {
                break;
            }
        }
        self.expect_p(")")?;
        Ok((params, rest))
    }

    fn parse_function_params_body(&mut self) -> R<FuncDef> {
        let (params, rest) = self.parse_params()?;
        let body = self.parse_block_body()?;
        Ok(FuncDef { name: None, params, rest, body: FuncBody::Block(body), arrow: false, implicit_it: false, implicit_return: false, generator: false, is_async: false })
    }

    /// After `function`: optional `*`, optional name, params, body.
    fn parse_function_rest(&mut self, expr: bool, is_async: bool) -> R<FuncDef> {
        let generator = self.eat_p("*");
        let name = match self.peek().clone() {
            T::Ident(n) => {
                self.next();
                Some(n)
            }
            _ if expr => None,
            _ => return self.err("expected function name"),
        };
        let mut f = self.parse_method_body(generator, is_async)?;
        f.name = name;
        Ok(f)
    }

    fn parse_arrow_body(&mut self, params: Vec<Param>, rest: Option<Pat>) -> R<Expr> {
        self.parse_arrow_body_async(params, rest, false)
    }

    fn parse_arrow_body_async(&mut self, params: Vec<Param>, rest: Option<Pat>, is_async: bool) -> R<Expr> {
        let (sg, sa) = (self.in_gen, self.in_async);
        self.in_gen = false;
        self.in_async = is_async;
        let body = if self.is_p("{") {
            self.parse_block_body().map(FuncBody::Block)
        } else {
            let save = self.no_in;
            self.no_in = false;
            let e = self.parse_assign();
            self.no_in = save;
            e.map(|e| FuncBody::Expr(Box::new(e)))
        };
        self.in_gen = sg;
        self.in_async = sa;
        let body = body?;
        Ok(Expr::Func(Arc::new(FuncDef { name: None, params, rest, body, arrow: true, implicit_it: false, implicit_return: false, generator: false, is_async })))
    }

    /// Groovy closure `{ a, b -> ... }` (current token is `{`).
    fn parse_closure(&mut self) -> R<Expr> {
        self.expect_p("{")?;
        let mut params = Vec::new();
        let mut implicit_it = true;
        // detect parameter list
        let mut n = 0;
        let mut has_arrow = false;
        loop {
            match self.peek_at(n) {
                T::P("->") => {
                    has_arrow = true;
                    break;
                }
                T::Ident(_) | T::P(",") | T::P("=") | T::P("<") | T::P(">") | T::P(".") | T::P("[") | T::P("]") => n += 1,
                T::Int(_) | T::Str(_) | T::Num(_) if self.is_p_at(n.saturating_sub(1), "=") => n += 1,
                _ => break,
            }
        }
        if has_arrow {
            implicit_it = false;
            while !self.is_p("->") {
                self.skip_groovy_type();
                let name = self.ident()?;
                let default = if self.eat_p("=") { Some(self.parse_cond()?) } else { None };
                params.push(Param { pat: Pat::Id(name), default });
                if !self.eat_p(",") {
                    break;
                }
            }
            self.expect_p("->")?;
        }
        let mut body = Vec::new();
        while !self.is_p("}") {
            if self.at_eof() {
                return self.err("expected '}' to close closure but found end of input");
            }
            if self.eat_p(";") {
                continue;
            }
            body.push(self.parse_statement()?);
        }
        self.next();
        Ok(Expr::Func(Arc::new(FuncDef { name: None, params, rest: None, body: FuncBody::Block(body), arrow: false, implicit_it, implicit_return: true, generator: false, is_async: false })))
    }

    // ------------------------------------------------------------------ expressions
    fn parse_sequence(&mut self) -> R<Expr> {
        let e = self.parse_expr()?;
        if self.is_p(",") && self.d == Dialect::Ognl {
            let mut v = vec![e];
            while self.eat_p(",") {
                v.push(self.parse_expr()?);
            }
            return Ok(Expr::Seq(v));
        }
        Ok(e)
    }

    fn parse_expr(&mut self) -> R<Expr> {
        let e = self.parse_assign()?;
        if self.d == Dialect::Js && self.is_p(",") {
            let mut v = vec![e];
            while self.eat_p(",") {
                v.push(self.parse_assign()?);
            }
            return Ok(Expr::Seq(v));
        }
        Ok(e)
    }

    fn is_arrow_ahead(&self) -> bool {
        // current token is `(`; find matching `)` and check for `=>`
        let mut depth = 0;
        let mut n = 0;
        loop {
            match self.peek_at(n) {
                T::P("(") | T::P("[") | T::P("{") => depth += 1,
                T::P(")") | T::P("]") | T::P("}") => {
                    depth -= 1;
                    if depth == 0 {
                        return self.is_p_at(n + 1, "=>");
                    }
                }
                T::Eof => return false,
                _ => {}
            }
            n += 1;
        }
    }

    fn parse_assign(&mut self) -> R<Expr> {
        if self.d == Dialect::Java && ((matches!(self.peek(), T::Ident(_)) && self.is_p_at(1, "->")) || (self.is_p("(") && self.is_java_lambda_paren())) {
            return self.parse_java_lambda();
        }
        if self.d == Dialect::Js && self.in_gen && self.is_kw("yield") {
            let pos = self.pos();
            self.next();
            let delegate = !self.nl() && self.eat_p("*");
            let ends = self.nl() && !delegate || matches!(self.peek(), T::P(")") | T::P("]") | T::P("}") | T::P(",") | T::P(";") | T::P(":") | T::Eof);
            let arg = if ends { None } else { Some(Box::new(self.parse_assign()?)) };
            return Ok(Expr::Yield { arg, delegate, pos });
        }
        if self.d == Dialect::Js && self.is_kw("async") && !self.tok_at(1).nl {
            // async arrow functions
            if matches!(self.peek_at(1), T::Ident(w) if !JS_KEYWORDS.contains(&w.as_str())) && self.is_p_at(2, "=>") {
                self.next();
                let name = self.ident()?;
                self.next();
                return self.parse_arrow_body_async(vec![Param { pat: Pat::Id(name), default: None }], None, true);
            }
            if self.is_p_at(1, "(") {
                let save = self.i;
                self.next();
                if self.is_arrow_ahead() {
                    let (params, rest) = self.parse_params()?;
                    self.expect_p("=>")?;
                    return self.parse_arrow_body_async(params, rest, true);
                }
                self.i = save;
            }
        }
        if self.d == Dialect::Js {
            if matches!(self.peek(), T::Ident(w) if !JS_KEYWORDS.contains(&w.as_str())) && self.is_p_at(1, "=>") {
                let name = self.ident()?;
                self.next();
                return self.parse_arrow_body(vec![Param { pat: Pat::Id(name), default: None }], None);
            }
            if self.is_p("(") && self.is_arrow_ahead() {
                let (params, rest) = self.parse_params()?;
                self.expect_p("=>")?;
                return self.parse_arrow_body(params, rest);
            }
        }
        let pos = self.pos();
        let lhs = self.parse_cond()?;
        let op = match self.peek() {
            T::P("=") => Some(None),
            T::P("+=") => Some(Some(AssignOp::Bin(BinOp::Add))),
            T::P("-=") => Some(Some(AssignOp::Bin(BinOp::Sub))),
            T::P("*=") => Some(Some(AssignOp::Bin(BinOp::Mul))),
            T::P("/=") => Some(Some(AssignOp::Bin(BinOp::Div))),
            T::P("%=") => Some(Some(AssignOp::Bin(BinOp::Mod))),
            T::P("**=") => Some(Some(AssignOp::Bin(BinOp::Pow))),
            T::P("<<=") => Some(Some(AssignOp::Bin(BinOp::Shl))),
            T::P(">>=") => Some(Some(AssignOp::Bin(BinOp::Shr))),
            T::P(">>>=") => Some(Some(AssignOp::Bin(BinOp::UShr))),
            T::P("&&=") => Some(Some(AssignOp::And)),
            T::P("||=") => Some(Some(AssignOp::Or)),
            T::P("??=") => Some(Some(AssignOp::Nullish)),
            _ => None,
        };
        if let Some(op) = op {
            if self.d == Dialect::Spel && op.is_none() {
                // SpEL supports assignment too
            }
            self.next();
            let target = self.to_pat(lhs)?;
            let value = self.parse_assign()?;
            return Ok(Expr::Assign { op, target: Box::new(target), value: Box::new(value), pos });
        }
        Ok(lhs)
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
                        Elem::Hole => v.push(None),
                        Elem::Spread(e) => rest = Some(Box::new(self.to_pat(e)?)),
                        Elem::Item(Expr::Assign { op: None, target, value, .. }) => v.push(Some((*target, Some(*value)))),
                        Elem::Item(e) => v.push(Some((self.to_pat(e)?, None))),
                    }
                }
                Pat::Arr(v, rest)
            }
            Expr::Object(props) => {
                let mut v = Vec::new();
                let mut rest = None;
                for p in props {
                    match p {
                        Prop::Spread(e) => rest = Some(Box::new(self.to_pat(e)?)),
                        Prop::KV(k, Expr::Assign { op: None, target, value, .. }) => v.push((k, *target, Some(*value))),
                        Prop::KV(k, e) => v.push((k, self.to_pat(e)?, None)),
                        Prop::Get(..) | Prop::Set(..) => return self.err("invalid destructuring target"),
                    }
                }
                Pat::Obj(v, rest)
            }
            _ => return self.err("invalid assignment target"),
        })
    }

    fn parse_cond(&mut self) -> R<Expr> {
        let c = self.parse_binary(1)?;
        if self.is_p("?:") {
            self.next();
            let b = self.parse_assign()?;
            return Ok(Expr::Elvis(Box::new(c), Box::new(b)));
        }
        if self.is_p("?") {
            self.next();
            let save = self.no_in;
            self.no_in = false;
            let a = self.parse_assign()?;
            self.no_in = save;
            self.expect_p(":")?;
            let b = self.parse_assign()?;
            return Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    /// Binary operator at the current position: (precedence, operator, token count).
    fn binop(&self) -> Option<(u8, BinKind, usize)> {
        use BinKind::*;
        let d = self.d;
        let t = self.peek();
        let r = match t {
            T::P("||") => (1, Or, 1),
            T::P("??") => (1, Nullish, 1),
            T::P("&&") => (2, And, 1),
            T::P("|") => (3, B(BinOp::BitOr), 1),
            T::P("^") => (4, B(BinOp::BitXor), 1),
            T::P("&") => (5, B(BinOp::BitAnd), 1),
            T::P("==") => (6, B(BinOp::Eq), 1),
            T::P("!=") => (6, B(BinOp::Ne), 1),
            T::P("===") => (6, B(BinOp::StrictEq), 1),
            T::P("!==") => (6, B(BinOp::StrictNe), 1),
            T::P("=~") => (6, B(BinOp::Find), 1),
            T::P("==~") => (6, B(BinOp::Matches), 1),
            T::P("<=>") => (6, B(BinOp::Cmp), 1),
            T::P("<") => (7, B(BinOp::Lt), 1),
            T::P(">") => (7, B(BinOp::Gt), 1),
            T::P("<=") => (7, B(BinOp::Le), 1),
            T::P(">=") => (7, B(BinOp::Ge), 1),
            T::P("!") if d == Dialect::Groovy && self.is_kw_at(1, "in") && !self.tok_at(1).nl => (7, B(BinOp::NotIn), 2),
            T::P("!") if d == Dialect::Groovy && self.is_kw_at(1, "instanceof") => (7, TypeTest(true), 2),
            T::P("<<") => (8, B(BinOp::Shl), 1),
            T::P(">>") => (8, B(BinOp::Shr), 1),
            T::P(">>>") => (8, B(BinOp::UShr), 1),
            T::P("..") => (8, Range(false), 1),
            T::P("..<") => (8, Range(true), 1),
            T::P("+") => (9, B(BinOp::Add), 1),
            T::P("-") => (9, B(BinOp::Sub), 1),
            T::P("*") => (10, B(BinOp::Mul), 1),
            T::P("/") => (10, B(BinOp::Div), 1),
            T::P("%") => (10, B(BinOp::Mod), 1),
            T::P("**") => (11, B(BinOp::Pow), 1),
            T::Ident(w) => match (w.as_str(), d) {
                ("instanceof", Dialect::Js) => (7, B(BinOp::InstanceOf), 1),
                ("instanceof", _) => (7, TypeTest(false), 1),
                ("in", Dialect::Js | Dialect::Groovy | Dialect::Ognl) if !self.no_in => (7, B(BinOp::In), 1),
                ("in", Dialect::Mvel) if !self.no_in => (7, MvelProject, 1),
                ("as", Dialect::Groovy) => (7, As, 1),
                ("not", Dialect::Ognl) if self.is_kw_at(1, "in") => (7, B(BinOp::NotIn), 2),
                ("and", Dialect::Ognl | Dialect::Spel | Dialect::Mvel) => (2, And, 1),
                ("or", Dialect::Ognl | Dialect::Spel | Dialect::Mvel) => (1, Or, 1),
                ("eq", Dialect::Ognl | Dialect::Spel) => (6, B(BinOp::Eq), 1),
                ("ne" | "neq", Dialect::Ognl | Dialect::Spel) => (6, B(BinOp::Ne), 1),
                ("lt", Dialect::Ognl | Dialect::Spel) => (7, B(BinOp::Lt), 1),
                ("gt", Dialect::Ognl | Dialect::Spel) => (7, B(BinOp::Gt), 1),
                ("le" | "lte", Dialect::Ognl | Dialect::Spel) => (7, B(BinOp::Le), 1),
                ("ge" | "gte", Dialect::Ognl | Dialect::Spel) => (7, B(BinOp::Ge), 1),
                ("div", Dialect::Spel) => (10, B(BinOp::Div), 1),
                ("mod", Dialect::Spel) => (10, B(BinOp::Mod), 1),
                ("matches", Dialect::Spel) => (7, B(BinOp::Matches), 1),
                ("between", Dialect::Spel) => (7, B(BinOp::Between), 1),
                ("contains", Dialect::Mvel) => (7, B(BinOp::Contains), 1),
                ("is", Dialect::Mvel) => (7, TypeTest(false), 1),
                ("bor", Dialect::Ognl) => (3, B(BinOp::BitOr), 1),
                ("xor", Dialect::Ognl) => (4, B(BinOp::BitXor), 1),
                ("band", Dialect::Ognl) => (5, B(BinOp::BitAnd), 1),
                _ => return None,
            },
            _ => return None,
        };
        // Groovy: a binary operator at the start of a line ends the statement
        if d == Dialect::Groovy && self.nl() && !matches!(t, T::P("&&") | T::P("||")) {
            return None;
        }
        Some(r)
    }

    fn parse_binary(&mut self, min: u8) -> R<Expr> {
        let mut left = self.parse_unary()?;
        while let Some((prec, kind, ntok)) = self.binop() {
            if prec < min {
                break;
            }
            let pos = self.pos();
            for _ in 0..ntok {
                self.next();
            }
            left = match kind {
                BinKind::As => {
                    let t = self.type_name()?;
                    Expr::As(Box::new(left), t, pos)
                }
                BinKind::TypeTest(neg) => {
                    if self.is_kw("final") {
                        self.next();
                    }
                    let t = self.type_name()?;
                    if self.d == Dialect::Java && !neg {
                        if let T::Ident(n) = self.peek().clone() {
                            if !matches!(n.as_str(), "instanceof") {
                                self.next();
                                left = Expr::InstanceBind { expr: Box::new(left), ty: t, name: n, pos };
                                continue;
                            }
                        }
                    }
                    let e = Expr::Bin(BinOp::InstanceOf, Box::new(left), Box::new(Expr::Type(t)), pos);
                    if neg {
                        Expr::Unary(UnOp::Not, Box::new(e), pos)
                    } else {
                        e
                    }
                }
                _ => {
                    let next_min = if prec == 11 { prec } else { prec + 1 };
                    let right = self.parse_binary(next_min)?;
                    match kind {
                        BinKind::And => Expr::And(Box::new(left), Box::new(right)),
                        BinKind::Or => Expr::Or(Box::new(left), Box::new(right)),
                        BinKind::Nullish => Expr::Nullish(Box::new(left), Box::new(right)),
                        BinKind::Range(ex) => Expr::Range { from: Box::new(left), to: Box::new(right), exclusive: ex },
                        BinKind::MvelProject => Expr::Project { obj: Box::new(right), expr: Box::new(left) },
                        BinKind::B(op) => {
                            // MVEL `x == empty`
                            if self.d == Dialect::Mvel && matches!(op, BinOp::Eq | BinOp::Ne) {
                                let is_empty = |e: &Expr| matches!(e, Expr::Ident(n, _) if n == "empty");
                                if is_empty(&right) || is_empty(&left) {
                                    let subject = if is_empty(&right) { left } else { right };
                                    let call = Expr::Call { callee: Box::new(Expr::Ident("__isEmpty".into(), pos)), args: vec![Elem::Item(subject)], optional: false, pos };
                                    return Ok(if op == BinOp::Ne { Expr::Unary(UnOp::Not, Box::new(call), pos) } else { call }).and_then(|e| self.continue_binary(e, min));
                                }
                            }
                            Expr::Bin(op, Box::new(left), Box::new(right), pos)
                        }
                        BinKind::As | BinKind::TypeTest(_) => unreachable!(),
                    }
                }
            };
        }
        Ok(left)
    }

    fn continue_binary(&mut self, mut left: Expr, min: u8) -> R<Expr> {
        while let Some((prec, BinKind::And | BinKind::Or, _)) = self.binop() {
            if prec < min {
                break;
            }
            let is_and = matches!(self.binop(), Some((_, BinKind::And, _)));
            self.next();
            let right = self.parse_binary(prec + 1)?;
            left = if is_and { Expr::And(Box::new(left), Box::new(right)) } else { Expr::Or(Box::new(left), Box::new(right)) };
        }
        Ok(left)
    }

    /// A type name (`Integer`, `java.lang.String`, `List<String>`, `int[]`, SpEL `T(Integer)`).
    fn type_name(&mut self) -> R<String> {
        if self.d == Dialect::Spel && self.is_kw("T") && self.is_p_at(1, "(") {
            self.next();
            self.next();
            let n = self.qualified_name()?;
            self.expect_p(")")?;
            return Ok(n.rsplit('.').next().unwrap_or(&n).to_string());
        }
        let n = self.qualified_name()?;
        if self.is_p("<") {
            let mut depth = 0;
            loop {
                match self.next() {
                    T::P("<") => depth += 1,
                    T::P(">") => depth -= 1,
                    T::P(">>") => depth -= 2,
                    T::Eof => break,
                    _ => {}
                }
                if depth <= 0 {
                    break;
                }
            }
        }
        let mut n = n.rsplit('.').next().unwrap_or(&n).to_string();
        while self.is_p("[") && self.is_p_at(1, "]") {
            self.next();
            self.next();
            n.push_str("[]");
        }
        Ok(n)
    }

    fn parse_unary(&mut self) -> R<Expr> {
        let pos = self.pos();
        let op = match self.peek() {
            T::P("!") => Some(UnOp::Not),
            T::P("-") => Some(UnOp::Neg),
            T::P("+") => Some(UnOp::Plus),
            T::P("~") if self.d != Dialect::Groovy => Some(UnOp::BitNot),
            T::Ident(w) if w == "typeof" && self.d == Dialect::Js => Some(UnOp::Typeof),
            T::Ident(w) if w == "void" && self.d == Dialect::Js => Some(UnOp::Void),
            T::Ident(w) if w == "delete" && self.d == Dialect::Js => Some(UnOp::Delete),
            T::Ident(w) if w == "not" && matches!(self.d, Dialect::Ognl | Dialect::Spel) => Some(UnOp::Not),
            T::Ident(w) if w == "await" && self.d == Dialect::Js && self.in_async && !matches!(self.peek_at(1), T::P(")") | T::P(";") | T::P(",") | T::P("=") | T::Eof) => {
                self.next();
                let e = self.parse_unary()?;
                return Ok(Expr::Await(Box::new(e), pos));
            }
            _ => None,
        };
        if let Some(op) = op {
            self.next();
            let e = self.parse_unary()?;
            // fold negative literals
            if op == UnOp::Neg {
                match e {
                    Expr::Int(i) => return Ok(Expr::Int(-i)),
                    Expr::Num(f) => return Ok(Expr::Num(-f)),
                    _ => {}
                }
            }
            return Ok(Expr::Unary(op, Box::new(e), pos));
        }
        if self.d == Dialect::Java && self.is_p("(") && self.is_java_cast() {
            self.next();
            let t = self.type_name()?;
            self.expect_p(")")?;
            let e = self.parse_unary()?;
            return Ok(match t.as_str() {
                "Object" | "Comparable" | "CharSequence" | "Number" | "Iterable" | "Collection" => e,
                _ => Expr::As(Box::new(e), t, pos),
            });
        }
        if self.d == Dialect::Groovy && self.is_p("~") {
            // pattern operator ~"regex" / ~/regex/
            self.next();
            let e = self.parse_unary()?;
            return Ok(Expr::Call { callee: Box::new(Expr::Ident("__pattern".into(), pos)), args: vec![Elem::Item(e)], optional: false, pos });
        }
        if self.is_p("++") || self.is_p("--") {
            let inc = self.is_p("++");
            self.next();
            let target = self.parse_unary()?;
            return Ok(Expr::Update { inc, prefix: true, target: Box::new(target), pos });
        }
        let e = self.parse_postfix()?;
        if (self.is_p("++") || self.is_p("--")) && !self.nl() {
            let inc = self.is_p("++");
            self.next();
            return Ok(Expr::Update { inc, prefix: false, target: Box::new(e), pos });
        }
        Ok(e)
    }

    fn parse_args(&mut self) -> R<Vec<Elem>> {
        self.expect_p("(")?;
        let mut args = Vec::new();
        let mut named: Vec<Prop> = Vec::new();
        let save = self.no_in;
        self.no_in = false;
        while !self.is_p(")") {
            if self.eat_p("...") || (self.d == Dialect::Groovy && self.is_p("*") && self.eat_p("*")) {
                args.push(Elem::Spread(self.parse_assign()?));
            } else if self.d == Dialect::Groovy && matches!(self.peek(), T::Ident(_) | T::Str(_)) && self.is_p_at(1, ":") {
                let k = match self.next() {
                    T::Ident(s) | T::Str(s) => s,
                    _ => unreachable!(),
                };
                self.next();
                let v = self.parse_assign()?;
                named.push(Prop::KV(PropKey::Name(k), v));
            } else {
                args.push(Elem::Item(self.parse_assign()?));
            }
            if !self.eat_p(",") {
                break;
            }
        }
        self.no_in = save;
        self.expect_p(")")?;
        if !named.is_empty() {
            args.insert(0, Elem::Item(Expr::Object(named)));
        }
        Ok(args)
    }

    fn parse_postfix(&mut self) -> R<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            let pos = self.pos();
            let nl = self.nl();
            match self.peek().clone() {
                T::P("::") => {
                    // method reference `Type::method`, `obj::method`, `Type::new`
                    self.next();
                    let name = self.ident()?;
                    e = Expr::MethodRef(Box::new(e), name, pos);
                    continue;
                }
                T::P(".") | T::P("?.") | T::P(".?") => {
                    let optional = !self.is_p(".");
                    self.next();
                    // OGNL projection / selection: list.{ expr } / .{? expr }
                    if self.d == Dialect::Ognl && self.is_p("{") {
                        self.next();
                        let mode = if self.eat_p("?") {
                            Some(SelectMode::All)
                        } else if self.eat_p("^") {
                            Some(SelectMode::First)
                        } else if self.is_kw("$") {
                            self.next();
                            Some(SelectMode::Last)
                        } else {
                            None
                        };
                        let inner = self.parse_expr()?;
                        self.expect_p("}")?;
                        e = match mode {
                            Some(m) => Expr::Select { obj: Box::new(e), expr: Box::new(inner), mode: m },
                            None => Expr::Project { obj: Box::new(e), expr: Box::new(inner) },
                        };
                        continue;
                    }
                    if self.d == Dialect::Js && optional && self.is_p("(") {
                        let args = self.parse_args()?;
                        e = Expr::Call { callee: Box::new(e), args, optional: true, pos };
                        continue;
                    }
                    let name = self.prop_name()?;
                    e = Expr::Member { obj: Box::new(e), name, optional, pos };
                }
                T::P("?.[") => {
                    self.next();
                    let idx = self.parse_expr()?;
                    self.expect_p("]")?;
                    e = Expr::Index { obj: Box::new(e), index: Box::new(idx), optional: true, pos };
                }
                T::P("*.") => {
                    self.next();
                    let name = self.prop_name()?;
                    if self.is_p("(") && !self.nl() {
                        let args = self.parse_args()?;
                        let it = Expr::Ident("it".into(), pos);
                        let call = Expr::Call { callee: Box::new(Expr::Member { obj: Box::new(it), name, optional: true, pos }), args, optional: false, pos };
                        let f = Expr::Func(Arc::new(FuncDef {
                            name: None,
                            params: vec![],
                            rest: None,
                            body: FuncBody::Expr(Box::new(call)),
                            arrow: false,
                            implicit_it: true,
                            implicit_return: true,
                            generator: false,
                            is_async: false,
                        }));
                        e = Expr::Call { callee: Box::new(Expr::Member { obj: Box::new(e), name: "collect".into(), optional: true, pos }), args: vec![Elem::Item(f)], optional: false, pos };
                    } else {
                        e = Expr::SpreadMember { obj: Box::new(e), name };
                    }
                }
                T::P(".?[") | T::P(".^[") | T::P(".$[") | T::P(".![") => {
                    let p = match self.next() {
                        T::P(p) => p,
                        _ => unreachable!(),
                    };
                    let inner = self.parse_expr()?;
                    self.expect_p("]")?;
                    e = match p {
                        ".![" => Expr::Project { obj: Box::new(e), expr: Box::new(inner) },
                        ".?[" => Expr::Select { obj: Box::new(e), expr: Box::new(inner), mode: SelectMode::All },
                        ".^[" => Expr::Select { obj: Box::new(e), expr: Box::new(inner), mode: SelectMode::First },
                        _ => Expr::Select { obj: Box::new(e), expr: Box::new(inner), mode: SelectMode::Last },
                    };
                }
                T::P("[") if !(nl && self.d == Dialect::Groovy) => {
                    self.next();
                    let save = self.no_in;
                    self.no_in = false;
                    let idx = self.parse_expr()?;
                    // Groovy list[1, 2]
                    let idx = if self.d == Dialect::Groovy && self.is_p(",") {
                        let mut items = vec![Elem::Item(idx)];
                        while self.eat_p(",") {
                            items.push(Elem::Item(self.parse_expr()?));
                        }
                        Expr::Array(items)
                    } else {
                        idx
                    };
                    self.no_in = save;
                    self.expect_p("]")?;
                    e = Expr::Index { obj: Box::new(e), index: Box::new(idx), optional: false, pos };
                }
                T::P("(") if !(nl && self.d == Dialect::Groovy) => {
                    let args = self.parse_args()?;
                    e = Expr::Call { callee: Box::new(e), args, optional: false, pos };
                }
                T::P("{") if self.d == Dialect::Groovy && !nl && self.closure_can_follow(&e) => {
                    let c = self.parse_closure()?;
                    e = match e {
                        Expr::Call { callee, mut args, optional, pos } => {
                            args.push(Elem::Item(c));
                            Expr::Call { callee, args, optional, pos }
                        }
                        other => Expr::Call { callee: Box::new(other), args: vec![Elem::Item(c)], optional: false, pos },
                    };
                }
                T::Tpl(_) if self.d == Dialect::Js && !nl => {
                    // tagged templates are not supported
                    return self.err("tagged template literals are not supported");
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn closure_can_follow(&self, e: &Expr) -> bool {
        matches!(e, Expr::Call { .. } | Expr::Member { .. } | Expr::Ident(..))
    }

    fn template(&mut self, parts: Vec<RawPart>) -> R<Expr> {
        let mut out = Vec::new();
        for p in parts {
            match p {
                RawPart::Lit(s) => out.push(TplPart::Lit(s)),
                RawPart::Code(code, off) => out.push(TplPart::Expr(parse_embedded(&code, off, self.d)?, None, None)),
            }
        }
        Ok(Expr::Template(out))
    }

    fn parse_primary(&mut self) -> R<Expr> {
        let pos = self.pos();
        let d = self.d;
        match self.peek().clone() {
            T::Int(i) => {
                self.next();
                Ok(Expr::Int(i))
            }
            T::Num(f) => {
                self.next();
                Ok(Expr::Num(f))
            }
            T::Str(s) => {
                self.next();
                Ok(Expr::Str(s))
            }
            T::Tpl(parts) => {
                self.next();
                self.template(parts)
            }
            T::Regex(s, f) => {
                self.next();
                Ok(Expr::Regex(s, f))
            }
            T::P("(") => {
                self.next();
                let save = self.no_in;
                self.no_in = false;
                let e = self.parse_expr()?;
                self.no_in = save;
                self.expect_p(")")?;
                Ok(e)
            }
            T::P("[") => {
                self.next();
                self.parse_array_or_map()
            }
            T::P("{") => match d {
                Dialect::Js => {
                    self.next();
                    self.parse_object_literal()
                }
                Dialect::Groovy => self.parse_closure(),
                Dialect::Ognl | Dialect::Mvel | Dialect::Java => {
                    self.next();
                    let mut items = Vec::new();
                    while !self.is_p("}") {
                        items.push(Elem::Item(self.parse_assign()?));
                        if !self.eat_p(",") {
                            break;
                        }
                    }
                    self.expect_p("}")?;
                    Ok(Expr::Array(items))
                }
                Dialect::Spel => {
                    self.next();
                    if self.eat_p(":") {
                        self.expect_p("}")?;
                        return Ok(Expr::Object(vec![]));
                    }
                    if self.eat_p("}") {
                        return Ok(Expr::Array(vec![]));
                    }
                    let first = self.parse_assign()?;
                    if self.eat_p(":") {
                        let mut props = Vec::new();
                        let key = spel_key(first);
                        props.push(Prop::KV(key, self.parse_assign()?));
                        while self.eat_p(",") {
                            let k = spel_key(self.parse_assign()?);
                            self.expect_p(":")?;
                            props.push(Prop::KV(k, self.parse_assign()?));
                        }
                        self.expect_p("}")?;
                        return Ok(Expr::Object(props));
                    }
                    let mut items = vec![Elem::Item(first)];
                    while self.eat_p(",") {
                        items.push(Elem::Item(self.parse_assign()?));
                    }
                    self.expect_p("}")?;
                    Ok(Expr::Array(items))
                }
                _ => self.unexpected(),
            },
            T::P("#{") => {
                // OGNL map literal
                self.next();
                let mut props = Vec::new();
                while !self.is_p("}") {
                    let k = self.parse_cond()?;
                    self.expect_p(":")?;
                    let v = self.parse_assign()?;
                    props.push(Prop::KV(spel_key(k), v));
                    if !self.eat_p(",") {
                        break;
                    }
                }
                self.expect_p("}")?;
                Ok(Expr::Object(props))
            }
            T::P("@") if d == Dialect::Ognl && matches!(self.peek_at(1), T::Ident(_)) => {
                // static member access `@java.lang.Math@max(1, 2)`, `@Integer@MAX_VALUE`
                self.next();
                let cls = self.qualified_name()?;
                self.expect_p("@")?;
                let mpos = self.pos();
                let name = self.ident()?;
                let simple = cls.rsplit('.').next().unwrap_or(&cls).to_string();
                let member = Expr::Member { obj: Box::new(Expr::Type(simple)), name, optional: false, pos: mpos };
                if self.is_p("(") {
                    let args = self.parse_args()?;
                    return Ok(Expr::Call { callee: Box::new(member), args, optional: false, pos });
                }
                Ok(member)
            }
            T::P("@") if d == Dialect::Spel => {
                self.next();
                let name = self.ident()?;
                Ok(Expr::Call { callee: Box::new(Expr::Ident("__bean".into(), pos)), args: vec![Elem::Item(Expr::Str(name))], optional: false, pos })
            }
            T::Ident(w) => {
                self.next();
                match w.as_str() {
                    "true" => return Ok(Expr::Bool(true)),
                    "false" => return Ok(Expr::Bool(false)),
                    "null" => return Ok(Expr::Null),
                    "nil" if d == Dialect::Mvel => return Ok(Expr::Null),
                    "undefined" if d == Dialect::Js => return Ok(Expr::Undef),
                    "this" => return Ok(Expr::This),
                    "super" => return Ok(Expr::Super(pos)),
                    "function" if d == Dialect::Js => {
                        let f = self.parse_function_rest(true, false)?;
                        return Ok(Expr::Func(Arc::new(f)));
                    }
                    "async" if d == Dialect::Js && self.is_kw("function") && !self.nl() => {
                        self.next();
                        let f = self.parse_function_rest(true, true)?;
                        return Ok(Expr::Func(Arc::new(f)));
                    }
                    "class" if d == Dialect::Js => {
                        self.i -= 1;
                        let c = self.parse_class()?;
                        return Ok(Expr::Class(Arc::new(c)));
                    }
                    "switch" if d == Dialect::Java => {
                        self.i -= 1;
                        return self.parse_java_switch(false);
                    }
                    "new" if d == Dialect::Java && self.java_array_new() => {
                        // `new int[3]`, `new String[]{"a"}`, `new int[2][3]`
                        let ty = self.qualified_name()?;
                        let mut dims = Vec::new();
                        while self.eat_p("[") {
                            if self.eat_p("]") {
                                dims.push(None);
                            } else {
                                dims.push(Some(self.parse_expr()?));
                                self.expect_p("]")?;
                            }
                        }
                        if self.is_p("{") {
                            return self.parse_primary();
                        }
                        let fill = if dims.len() > 1 {
                            Expr::Null
                        } else {
                            match ty.as_str() {
                                "int" | "long" | "short" | "byte" => Expr::Int(0),
                                "double" | "float" => Expr::Num(0.0),
                                "boolean" => Expr::Bool(false),
                                "char" => Expr::Str("\0".into()),
                                _ => Expr::Null,
                            }
                        };
                        let size = dims.into_iter().next().flatten().unwrap_or(Expr::Int(0));
                        return Ok(Expr::Call { callee: Box::new(Expr::Ident("__newarray".into(), pos)), args: vec![Elem::Item(size), Elem::Item(fill)], optional: false, pos });
                    }
                    "new" if matches!(d, Dialect::Js | Dialect::Groovy | Dialect::Mvel | Dialect::Spel | Dialect::Java) => {
                        let callee = if d == Dialect::Js {
                            let mut c = self.parse_primary()?;
                            while self.is_p(".") {
                                self.next();
                                let mpos = self.pos();
                                let name = self.prop_name()?;
                                c = Expr::Member { obj: Box::new(c), name, optional: false, pos: mpos };
                            }
                            c
                        } else {
                            Expr::Type(self.type_name()?)
                        };
                        if d != Dialect::Js && self.is_p("<") {
                            // diamond `new ArrayList<>()`
                            self.next();
                            while !self.is_p(">") && !self.at_eof() {
                                self.next();
                            }
                            self.next();
                        }
                        let args = if self.is_p("(") { self.parse_args()? } else { vec![] };
                        let mut e = Expr::New { callee: Box::new(callee), args, pos };
                        // Groovy: new Foo() { ... } anonymous classes are not supported
                        if d == Dialect::Js {
                            e = self.continue_postfix(e)?;
                        }
                        return Ok(e);
                    }
                    "T" if d == Dialect::Spel && self.is_p("(") => {
                        self.next();
                        let n = self.qualified_name()?;
                        self.expect_p(")")?;
                        return Ok(Expr::Type(n.rsplit('.').next().unwrap_or(&n).to_string()));
                    }
                    _ => {}
                }
                // fully qualified Java class names: java.util.UUID, groovy.json.JsonOutput
                if self.groovy_like() && matches!(w.as_str(), "java" | "javax" | "groovy" | "org" | "com") && self.is_p(".") {
                    let save = self.i;
                    while self.is_p(".") && matches!(self.peek_at(1), T::Ident(x) if x.chars().next().is_some_and(|c| c.is_ascii_lowercase())) {
                        self.next();
                        self.next();
                    }
                    if self.is_p(".") && matches!(self.peek_at(1), T::Ident(x) if x.chars().next().is_some_and(|c| c.is_ascii_uppercase())) {
                        self.next();
                        let t = self.ident()?;
                        return Ok(Expr::Type(t));
                    }
                    self.i = save;
                }
                if matches!(d, Dialect::Js) && JS_KEYWORDS.contains(&w.as_str()) && !matches!(w.as_str(), "of") {
                    self.i -= 1;
                    return self.err(format!("unexpected keyword '{}'", w));
                }
                Ok(Expr::Ident(w, pos))
            }
            _ => self.unexpected(),
        }
    }

    /// After `new` (Java): whether an array creation follows.
    fn java_array_new(&self) -> bool {
        let mut n = 0;
        while matches!(self.peek_at(n), T::Ident(_)) {
            n += 1;
            if self.is_p_at(n, ".") {
                n += 1;
            } else {
                break;
            }
        }
        self.is_p_at(n, "[")
    }

    fn continue_postfix(&mut self, e: Expr) -> R<Expr> {
        // only member access / calls directly after `new X()`
        let mut e = e;
        loop {
            let pos = self.pos();
            if self.is_p(".") {
                self.next();
                let name = self.prop_name()?;
                e = Expr::Member { obj: Box::new(e), name, optional: false, pos };
            } else if self.is_p("(") {
                let args = self.parse_args()?;
                e = Expr::Call { callee: Box::new(e), args, optional: false, pos };
            } else {
                return Ok(e);
            }
        }
    }

    fn parse_array_or_map(&mut self) -> R<Expr> {
        let save = self.no_in;
        self.no_in = false;
        let r = self.parse_array_or_map_inner();
        self.no_in = save;
        r
    }

    fn parse_array_or_map_inner(&mut self) -> R<Expr> {
        let groovy = self.groovy_like();
        if groovy && self.is_p(":") && self.is_p_at(1, "]") {
            self.next();
            self.next();
            return Ok(Expr::Object(vec![]));
        }
        let is_map_entry = |p: &Parser| groovy && matches!(p.peek(), T::Ident(_) | T::Str(_) | T::Int(_) | T::Num(_)) && p.is_p_at(1, ":");
        if is_map_entry(self) || (groovy && self.is_p("(") && self.paren_then_colon()) {
            let mut props = Vec::new();
            while !self.is_p("]") {
                let key = if self.is_p("(") {
                    self.next();
                    let k = self.parse_expr()?;
                    self.expect_p(")")?;
                    PropKey::Computed(k)
                } else {
                    match self.next() {
                        T::Ident(s) | T::Str(s) => PropKey::Name(s),
                        T::Int(i) => PropKey::Name(i.to_string()),
                        T::Num(f) => PropKey::Name(f.to_string()),
                        _ => return self.err("expected map key"),
                    }
                };
                self.expect_p(":")?;
                let v = self.parse_assign()?;
                props.push(Prop::KV(key, v));
                if !self.eat_p(",") {
                    break;
                }
            }
            self.expect_p("]")?;
            return Ok(Expr::Object(props));
        }
        let mut items = Vec::new();
        while !self.is_p("]") {
            if self.d == Dialect::Js && self.is_p(",") {
                self.next();
                items.push(Elem::Hole);
                continue;
            }
            if self.eat_p("...") || (self.d == Dialect::Groovy && self.is_p("*") && self.eat_p("*")) {
                items.push(Elem::Spread(self.parse_assign()?));
            } else {
                items.push(Elem::Item(self.parse_assign()?));
            }
            if !self.eat_p(",") {
                break;
            }
        }
        self.expect_p("]")?;
        Ok(Expr::Array(items))
    }

    fn paren_then_colon(&self) -> bool {
        let mut depth = 0;
        let mut n = 0;
        loop {
            match self.peek_at(n) {
                T::P("(") | T::P("[") | T::P("{") => depth += 1,
                T::P(")") | T::P("]") | T::P("}") => {
                    depth -= 1;
                    if depth == 0 {
                        return self.is_p_at(n + 1, ":");
                    }
                }
                T::Eof => return false,
                _ => {}
            }
            n += 1;
        }
    }

    fn parse_object_literal(&mut self) -> R<Expr> {
        let save = self.no_in;
        self.no_in = false;
        let mut props = Vec::new();
        while !self.is_p("}") {
            let accessor_or_method = (self.is_kw("get") || self.is_kw("set") || self.is_kw("async")) && matches!(self.peek_at(1), T::Ident(_) | T::Str(_) | T::Int(_) | T::P("[") | T::P("*"));
            if self.eat_p("...") {
                props.push(Prop::Spread(self.parse_assign()?));
            } else if accessor_or_method || self.is_p("*") {
                let (m, key) = self.parse_method_head()?;
                let mut f = self.parse_method_body(m.generator, m.is_async)?;
                if let PropKey::Name(n) = &key {
                    f.name = Some(n.clone());
                }
                let f = Arc::new(f);
                props.push(match m.accessor {
                    Some(true) => Prop::Get(key, f),
                    Some(false) => Prop::Set(key, f),
                    None => Prop::KV(key, Expr::Func(f)),
                });
            } else {
                let key = match self.peek().clone() {
                    T::Ident(s) => {
                        self.next();
                        PropKey::Name(s)
                    }
                    T::Str(s) => {
                        self.next();
                        PropKey::Name(s)
                    }
                    T::Int(i) => {
                        self.next();
                        PropKey::Name(i.to_string())
                    }
                    T::Num(f) => {
                        self.next();
                        PropKey::Name(crate::interp::js_num_str(f))
                    }
                    T::P("[") => {
                        self.next();
                        let k = self.parse_assign()?;
                        self.expect_p("]")?;
                        PropKey::Computed(k)
                    }
                    _ => return self.err(format!("unexpected {} in object literal", describe(self.peek()))),
                };
                if self.is_p("(") {
                    let mut f = self.parse_function_params_body()?;
                    if let PropKey::Name(n) = &key {
                        f.name = Some(n.clone());
                    }
                    props.push(Prop::KV(key, Expr::Func(Arc::new(f))));
                } else if self.eat_p(":") {
                    props.push(Prop::KV(key, self.parse_assign()?));
                } else {
                    // shorthand `{a}` or `{a = 1}` (pattern)
                    let PropKey::Name(n) = &key else { return self.err("expected ':' in object literal") };
                    let pos = self.pos();
                    let mut v = Expr::Ident(n.clone(), pos);
                    if self.eat_p("=") {
                        let def = self.parse_assign()?;
                        v = Expr::Assign { op: None, target: Box::new(Pat::Id(n.clone())), value: Box::new(def), pos };
                    }
                    props.push(Prop::KV(key, v));
                }
            }
            if !self.eat_p(",") {
                break;
            }
        }
        self.no_in = save;
        self.expect_p("}")?;
        Ok(Expr::Object(props))
    }
}

fn spel_key(e: Expr) -> PropKey {
    match e {
        Expr::Ident(n, _) => PropKey::Name(n),
        Expr::Str(s) => PropKey::Name(s),
        Expr::Int(i) => PropKey::Name(i.to_string()),
        other => PropKey::Computed(other),
    }
}

struct MethodHead {
    /// Some(true) getter, Some(false) setter.
    accessor: Option<bool>,
    generator: bool,
    is_async: bool,
}

#[derive(Clone, Copy)]
enum BinKind {
    B(BinOp),
    And,
    Or,
    Nullish,
    Range(bool),
    As,
    TypeTest(bool),
    MvelProject,
}
