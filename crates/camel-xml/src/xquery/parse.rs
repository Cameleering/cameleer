//! Scannerless recursive-descent parser for XQuery 1.0 main modules (plus a few XQuery
//! 3.x conveniences: `||`, `!`, `try`/`catch`, `group by`, `count`, external variable
//! defaults, empty enclosed expressions).

use crate::xpath::ast::{Axis, NodeTest};

use super::ast::*;
use super::types::{Atomic, AtomicType, Decimal, ItemType, Occ, SeqType, XqError, XqResult};
use super::{ERR_NS, FN_NS, LOCAL_NS, MATH_NS, XSI_NS, XS_NS};

type R<T> = XqResult<T>;

const KIND_TESTS: &[&str] = &["node", "text", "comment", "processing-instruction", "element", "attribute", "document-node", "schema-element", "schema-attribute", "item", "empty-sequence"];
const RESERVED_FUNCTIONS: &[&str] = &["if", "typeswitch", "switch"];

pub(crate) struct Parser<'a> {
    s: Vec<char>,
    i: usize,
    src: &'a str,
    ns: Vec<(String, String)>,
    default_elem_ns: String,
    default_fn_ns: String,
    boundary_preserve: bool,
    module: Module,
}

fn is_name_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '\u{B7}')
}

/// Parse a main module with extra namespace bindings.
pub(crate) fn parse_module(src: &str, bindings: &[(String, String)]) -> R<Module> {
    let mut ns: Vec<(String, String)> = vec![
        ("xml".into(), crate::dom::XML_NS.into()),
        ("xs".into(), XS_NS.into()),
        ("xsi".into(), XSI_NS.into()),
        ("fn".into(), FN_NS.into()),
        ("local".into(), LOCAL_NS.into()),
        ("err".into(), ERR_NS.into()),
        ("math".into(), MATH_NS.into()),
    ];
    ns.extend(bindings.iter().cloned());
    let mut p = Parser { s: src.chars().collect(), i: 0, src, ns, default_elem_ns: String::new(), default_fn_ns: FN_NS.into(), boundary_preserve: false, module: Module::default() };
    p.module_()?;
    Ok(p.module)
}

impl Parser<'_> {
    // ---------------------------------------------------------------- lexing helpers

    fn err<T>(&self, m: impl Into<String>) -> R<T> {
        let consumed: String = self.s[..self.i.min(self.s.len())].iter().collect();
        let line = consumed.matches('\n').count() + 1;
        let col = consumed.rsplit('\n').next().map(|l| l.chars().count()).unwrap_or(0) + 1;
        let near: String = self.s[self.i.min(self.s.len())..].iter().take(20).collect();
        let _ = self.src;
        Err(XqError::syntax(format!("{} at line {} column {} near '{}'", m.into(), line, col, near)))
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn peek_at(&self, k: usize) -> Option<char> {
        self.s.get(self.i + k).copied()
    }

    fn starts(&self, t: &str) -> bool {
        let mut k = self.i;
        for c in t.chars() {
            if self.s.get(k) != Some(&c) {
                return false;
            }
            k += 1;
        }
        true
    }

    /// Skip whitespace and `(: comments :)` (nested).
    fn ws(&mut self) {
        loop {
            while self.peek().is_some_and(|c| c.is_whitespace()) {
                self.i += 1;
            }
            if self.starts("(:") {
                let mut depth = 0;
                while self.i < self.s.len() {
                    if self.starts("(:") {
                        depth += 1;
                        self.i += 2;
                    } else if self.starts(":)") {
                        depth -= 1;
                        self.i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        self.i += 1;
                    }
                }
                continue;
            }
            break;
        }
    }

    /// After whitespace, does the text start with `t`?
    fn at(&mut self, t: &str) -> bool {
        self.ws();
        self.starts(t)
    }

    fn eat(&mut self, t: &str) -> bool {
        if self.at(t) {
            self.i += t.chars().count();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: &str) -> R<()> {
        if self.eat(t) {
            Ok(())
        } else {
            self.err(format!("Expected '{}'", t))
        }
    }

    /// After whitespace, a keyword (not followed by a name character)?
    fn at_kw(&mut self, kw: &str) -> bool {
        self.ws();
        self.starts(kw) && !self.s.get(self.i + kw.len()).is_some_and(|&c| is_name_char(c) || c == ':')
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.at_kw(kw) {
            self.i += kw.len();
            true
        } else {
            false
        }
    }

    /// Keywords in sequence (all or nothing).
    fn eat_kws(&mut self, kws: &[&str]) -> bool {
        let save = self.i;
        for k in kws {
            if !self.eat_kw(k) {
                self.i = save;
                return false;
            }
        }
        true
    }

    /// Lookahead: keyword followed (after whitespace) by character `c`.
    fn kw_then(&mut self, kw: &str, c: char) -> bool {
        let save = self.i;
        let r = self.eat_kw(kw) && {
            self.ws();
            self.peek() == Some(c)
        };
        self.i = save;
        r
    }

    fn ncname(&mut self) -> Option<String> {
        if !self.peek().is_some_and(is_name_start) {
            return None;
        }
        let st = self.i;
        while self.peek().is_some_and(is_name_char) {
            self.i += 1;
        }
        Some(self.s[st..self.i].iter().collect())
    }

    /// A QName as written: `(prefix, local)`; no whitespace allowed inside.
    fn qname_raw(&mut self) -> Option<(String, String)> {
        let save = self.i;
        let a = self.ncname()?;
        if self.peek() == Some(':') && self.peek_at(1).is_some_and(is_name_start) {
            self.i += 1;
            match self.ncname() {
                Some(b) => return Some((a, b)),
                None => {
                    self.i = save;
                    return None;
                }
            }
        }
        Some((String::new(), a))
    }

    fn expect_qname(&mut self) -> R<(String, String)> {
        self.ws();
        match self.qname_raw() {
            Some(q) => Ok(q),
            None => self.err("Expected a name"),
        }
    }

    fn lookup_prefix(&self, p: &str) -> R<String> {
        match self.ns.iter().rev().find(|(q, _)| q == p) {
            Some((_, u)) => Ok(u.clone()),
            None => Err(XqError::static_err("XPST0081", format!("Namespace prefix '{}' is not declared", p))),
        }
    }

    fn resolve_elem(&self, p: &str, l: &str) -> R<QN> {
        if p.is_empty() {
            Ok(QN::new(self.default_elem_ns.clone(), l))
        } else {
            Ok(QN::new(self.lookup_prefix(p)?, l))
        }
    }

    fn resolve_plain(&self, p: &str, l: &str) -> R<QN> {
        if p.is_empty() {
            Ok(QN::local(l))
        } else {
            Ok(QN::new(self.lookup_prefix(p)?, l))
        }
    }

    fn resolve_fn(&self, p: &str, l: &str) -> R<QN> {
        if p.is_empty() {
            Ok(QN::new(self.default_fn_ns.clone(), l))
        } else {
            Ok(QN::new(self.lookup_prefix(p)?, l))
        }
    }

    fn var_name(&mut self) -> R<QN> {
        self.expect("$")?;
        let (p, l) = self.expect_qname()?;
        self.resolve_plain(&p, &l)
    }

    fn string_lit(&mut self) -> R<String> {
        self.ws();
        let q = match self.peek() {
            Some(c @ ('"' | '\'')) => c,
            _ => return self.err("Expected a string literal"),
        };
        self.i += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return self.err("Unterminated string literal"),
                Some(c) if c == q => {
                    if self.peek_at(1) == Some(q) {
                        out.push(q);
                        self.i += 2;
                    } else {
                        self.i += 1;
                        return Ok(out);
                    }
                }
                Some('&') => out.push_str(&self.entity_ref()?),
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    /// `&lt;`, `&#10;`, `&#x20;`... (at `&`).
    fn entity_ref(&mut self) -> R<String> {
        let st = self.i;
        while self.peek().is_some_and(|c| c != ';') && self.i - st < 12 {
            self.i += 1;
        }
        if self.peek() != Some(';') {
            self.i = st;
            return self.err("Invalid entity reference");
        }
        let name: String = self.s[st + 1..self.i].iter().collect();
        self.i += 1;
        let r = match name.as_str() {
            "lt" => "<".to_string(),
            "gt" => ">".into(),
            "amp" => "&".into(),
            "quot" => "\"".into(),
            "apos" => "'".into(),
            n if n.starts_with("#x") => u32::from_str_radix(&n[2..], 16).ok().and_then(char::from_u32).map(|c| c.to_string()).ok_or_else(|| XqError::syntax(format!("Invalid character reference &{};", n)))?,
            n if n.starts_with('#') => n[1..].parse::<u32>().ok().and_then(char::from_u32).map(|c| c.to_string()).ok_or_else(|| XqError::syntax(format!("Invalid character reference &{};", n)))?,
            n => return Err(XqError::syntax(format!("Unknown entity reference &{};", n))),
        };
        Ok(r)
    }

    // ---------------------------------------------------------------- prolog

    fn module_(&mut self) -> R<()> {
        if self.eat_kws(&["xquery", "version"]) {
            self.string_lit()?;
            if self.eat_kw("encoding") {
                self.string_lit()?;
            }
            self.expect(";")?;
        } else if self.eat_kws(&["xquery", "encoding"]) {
            self.string_lit()?;
            self.expect(";")?;
        }
        if self.at_kw("module") && self.kw_after("module", "namespace") {
            return Err(XqError::static_err("XQST0009", "Library modules are not supported (a main module is required)"));
        }
        loop {
            if self.eat_kws(&["declare", "namespace"]) {
                let (_, p) = self.expect_qname()?;
                self.expect("=")?;
                let u = self.string_lit()?;
                self.ns.push((p, u));
                self.expect(";")?;
            } else if self.eat_kws(&["declare", "default", "element", "namespace"]) {
                self.default_elem_ns = self.string_lit()?;
                self.expect(";")?;
            } else if self.eat_kws(&["declare", "default", "function", "namespace"]) {
                self.default_fn_ns = self.string_lit()?;
                self.expect(";")?;
            } else if self.eat_kws(&["declare", "boundary-space"]) {
                if self.eat_kw("preserve") {
                    self.boundary_preserve = true;
                } else if self.eat_kw("strip") {
                    self.boundary_preserve = false;
                } else {
                    return self.err("Expected preserve or strip");
                }
                self.expect(";")?;
            } else if self.eat_kws(&["declare", "default", "order", "empty"]) {
                if self.eat_kw("greatest") {
                    self.module.empty_greatest = true;
                } else if !self.eat_kw("least") {
                    return self.err("Expected greatest or least");
                }
                self.expect(";")?;
            } else if self.eat_kws(&["declare", "variable"]) {
                let name = self.var_name()?;
                let ty = if self.eat_kw("as") { Some(self.seq_type()?) } else { None };
                let (external, value) = if self.eat_kw("external") {
                    (true, if self.eat(":=") { Some(self.expr_single()?) } else { None })
                } else {
                    self.expect(":=")?;
                    (false, Some(self.expr_single()?))
                };
                self.expect(";")?;
                if self.module.vars.iter().any(|v| v.name == name) {
                    return Err(XqError::static_err("XQST0049", format!("Variable ${} is declared twice", name.local)));
                }
                self.module.vars.push(VarDecl { name, ty, value, external });
            } else if self.eat_kws(&["declare", "function"]) || self.eat_kws(&["declare", "updating", "function"]) {
                self.function_decl()?;
            } else if self.eat_kws(&["declare", "option"]) {
                self.expect_qname()?;
                self.string_lit()?;
                self.expect(";")?;
            } else if self.at_kw("declare")
                && ["ordering", "construction", "copy-namespaces", "base-uri", "default", "decimal-format", "context"].iter().any(|k| self.kw_after("declare", k))
            {
                self.skip_to_semicolon()?;
            } else if self.at_kw("import") && (self.kw_after("import", "schema") || self.kw_after("import", "module")) {
                return Err(XqError::static_err("XQST0009", "import schema / import module are not supported"));
            } else {
                break;
            }
        }
        self.module.namespaces = self.ns.clone();
        self.ws();
        if self.i < self.s.len() {
            let body = self.expr()?;
            self.module.body = Some(body);
        }
        self.ws();
        if self.i < self.s.len() {
            return self.err("Unexpected text after the query body");
        }
        if self.module.body.is_none() {
            return Err(XqError::syntax("The query has no body"));
        }
        Ok(())
    }

    fn kw_after(&mut self, first: &str, second: &str) -> bool {
        let save = self.i;
        let r = self.eat_kw(first) && self.at_kw(second);
        self.i = save;
        r
    }

    fn skip_to_semicolon(&mut self) -> R<()> {
        loop {
            self.ws();
            match self.peek() {
                None => return self.err("Expected ';'"),
                Some(';') => {
                    self.i += 1;
                    return Ok(());
                }
                Some('"' | '\'') => {
                    self.string_lit()?;
                }
                _ => self.i += 1,
            }
        }
    }

    fn function_decl(&mut self) -> R<()> {
        let (p, l) = self.expect_qname()?;
        let name = self.resolve_fn(&p, &l)?;
        if name.uri.is_empty() || name.uri == FN_NS || name.uri == XS_NS {
            return Err(XqError::static_err("XQST0045", format!("Function {} must be declared in a non-reserved namespace (e.g. local:)", l)));
        }
        self.expect("(")?;
        let mut params = Vec::new();
        if !self.eat(")") {
            loop {
                let v = self.var_name()?;
                let ty = if self.eat_kw("as") { Some(self.seq_type()?) } else { None };
                if params.iter().any(|(q, _)| *q == v) {
                    return Err(XqError::static_err("XQST0039", format!("Duplicate parameter ${}", v.local)));
                }
                params.push((v, ty));
                if self.eat(")") {
                    break;
                }
                self.expect(",")?;
            }
        }
        let ret = if self.eat_kw("as") { Some(self.seq_type()?) } else { None };
        if self.eat_kw("external") {
            return Err(XqError::static_err("XPST0017", format!("External function {} is not available", l)));
        }
        let body = self.enclosed()?;
        self.expect(";")?;
        let key = (name.clone(), params.len());
        if self.module.functions.contains_key(&key) {
            return Err(XqError::static_err("XQST0034", format!("Function {}#{} is declared twice", l, params.len())));
        }
        self.module.functions.insert(key, Function { name, params, ret, body });
        Ok(())
    }

    /// `{ Expr? }`
    fn enclosed(&mut self) -> R<Expr> {
        self.expect("{")?;
        if self.eat("}") {
            return Ok(Expr::Seq(Vec::new()));
        }
        let e = self.expr()?;
        self.expect("}")?;
        Ok(e)
    }

    // ---------------------------------------------------------------- types

    fn seq_type(&mut self) -> R<SeqType> {
        if self.kw_then("empty-sequence", '(') {
            self.eat_kw("empty-sequence");
            self.expect("(")?;
            self.expect(")")?;
            return Ok(SeqType { item: ItemType::Empty, occ: Occ::Star });
        }
        let item = self.item_type()?;
        let occ = if self.starts("?") {
            self.i += 1;
            Occ::Opt
        } else if self.starts("*") {
            self.i += 1;
            Occ::Star
        } else if self.starts("+") {
            self.i += 1;
            Occ::Plus
        } else {
            // allow whitespace before the indicator
            let save = self.i;
            self.ws();
            match self.peek() {
                Some('?') if self.peek_at(1) != Some('?') => {
                    self.i += 1;
                    Occ::Opt
                }
                _ => {
                    self.i = save;
                    Occ::One
                }
            }
        };
        Ok(SeqType { item, occ })
    }

    fn item_type(&mut self) -> R<ItemType> {
        self.ws();
        if self.kw_then("item", '(') {
            self.eat_kw("item");
            self.expect("(")?;
            self.expect(")")?;
            return Ok(ItemType::Item);
        }
        for k in KIND_TESTS {
            if *k != "item" && *k != "empty-sequence" && self.kw_then(k, '(') {
                return self.kind_test();
            }
        }
        let (p, l) = self.expect_qname()?;
        self.atomic_type(&p, &l)
    }

    fn atomic_type(&self, p: &str, l: &str) -> R<ItemType> {
        let uri = if p.is_empty() { XS_NS.to_string() } else { self.lookup_prefix(p)? };
        if uri == XS_NS {
            if let Some(t) = AtomicType::from_local(l) {
                return Ok(ItemType::Atomic(t));
            }
        }
        Err(XqError::static_err("XPST0051", format!("Unknown atomic type {}{}", if p.is_empty() { String::new() } else { format!("{}:", p) }, l)))
    }

    fn single_type(&mut self) -> R<(AtomicType, bool)> {
        let (p, l) = self.expect_qname()?;
        let t = match self.atomic_type(&p, &l)? {
            ItemType::Atomic(t) => t,
            _ => unreachable!(),
        };
        let opt = self.eat("?");
        Ok((t, opt))
    }

    /// A kind test, at its keyword.
    fn kind_test(&mut self) -> R<ItemType> {
        self.ws();
        let name = self.ncname().unwrap_or_default();
        self.expect("(")?;
        let it = match name.as_str() {
            "node" => ItemType::AnyNode,
            "text" => ItemType::Text,
            "comment" => ItemType::Comment,
            "processing-instruction" => {
                self.ws();
                let t = match self.peek() {
                    Some('"' | '\'') => Some(self.string_lit()?),
                    Some(c) if is_name_start(c) => self.ncname(),
                    _ => None,
                };
                ItemType::Pi(t)
            }
            "element" | "attribute" => {
                self.ws();
                let mut n = None;
                if self.eat("*") {
                } else if self.peek().is_some_and(is_name_start) {
                    let (p, l) = self.expect_qname()?;
                    let q = if name == "element" { self.resolve_elem(&p, &l)? } else { self.resolve_plain(&p, &l)? };
                    n = Some((q.uri, q.local));
                }
                if self.eat(",") {
                    // type annotation: accepted, ignored (untyped data)
                    self.expect_qname()?;
                    self.eat("?");
                }
                if name == "element" {
                    ItemType::Element(n)
                } else {
                    ItemType::Attribute(n)
                }
            }
            "document-node" => {
                self.ws();
                if self.peek() == Some(')') {
                    ItemType::Document(None)
                } else {
                    let inner = self.kind_test()?;
                    ItemType::Document(Some(Box::new(inner)))
                }
            }
            other => return Err(XqError::static_err("XPST0008", format!("{}() tests are not supported (no schema support)", other))),
        };
        self.expect(")")?;
        Ok(it)
    }

    // ---------------------------------------------------------------- expressions

    pub(crate) fn expr(&mut self) -> R<Expr> {
        let first = self.expr_single()?;
        if !self.at(",") {
            return Ok(first);
        }
        let mut v = vec![first];
        while self.eat(",") {
            v.push(self.expr_single()?);
        }
        Ok(Expr::Seq(v))
    }

    fn expr_single(&mut self) -> R<Expr> {
        self.ws();
        if self.kw_then("for", '$') || self.kw_then("let", '$') {
            return self.flwor();
        }
        if self.kw_then("some", '$') || self.kw_then("every", '$') {
            let every = self.eat_kw("every");
            if !every {
                self.eat_kw("some");
            }
            let mut binds = Vec::new();
            loop {
                let v = self.var_name()?;
                if self.eat_kw("as") {
                    self.seq_type()?;
                }
                if !self.eat_kw("in") {
                    return self.err("Expected 'in'");
                }
                binds.push((v, self.expr_single()?));
                if !self.eat(",") {
                    break;
                }
            }
            if !self.eat_kw("satisfies") {
                return self.err("Expected 'satisfies'");
            }
            let sat = self.expr_single()?;
            return Ok(Expr::Quantified { every, binds, sat: Box::new(sat) });
        }
        if self.kw_then("if", '(') {
            self.eat_kw("if");
            self.expect("(")?;
            let c = self.expr()?;
            self.expect(")")?;
            if !self.eat_kw("then") {
                return self.err("Expected 'then'");
            }
            let t = self.expr_single()?;
            let e = if self.eat_kw("else") { self.expr_single()? } else { Expr::Seq(Vec::new()) };
            return Ok(Expr::If(Box::new(c), Box::new(t), Box::new(e)));
        }
        if self.kw_then("typeswitch", '(') {
            return self.typeswitch();
        }
        if self.kw_then("try", '{') {
            self.eat_kw("try");
            let body = self.enclosed()?;
            let mut catches = Vec::new();
            while self.eat_kw("catch") {
                let mut codes = Vec::new();
                loop {
                    self.ws();
                    if self.eat("*") {
                        codes.push(None);
                    } else {
                        let save = self.i;
                        let (p, l) = self.expect_qname()?;
                        if self.starts(":*") {
                            self.i = save;
                            self.ncname();
                            self.i += 2;
                            codes.push(None);
                        } else {
                            let _ = p;
                            codes.push(Some(l));
                        }
                    }
                    if !self.eat("|") {
                        break;
                    }
                }
                let body = self.enclosed()?;
                catches.push(Catch { codes, body });
            }
            if catches.is_empty() {
                return self.err("Expected 'catch'");
            }
            return Ok(Expr::TryCatch { body: Box::new(body), catches });
        }
        self.or_expr()
    }

    fn flwor(&mut self) -> R<Expr> {
        let mut clauses = Vec::new();
        loop {
            if self.kw_then("for", '$') {
                self.eat_kw("for");
                loop {
                    let var = self.var_name()?;
                    let ty = if self.eat_kw("as") { Some(self.seq_type()?) } else { None };
                    let pos = if self.eat_kw("at") { Some(self.var_name()?) } else { None };
                    if !self.eat_kw("in") {
                        return self.err("Expected 'in'");
                    }
                    let expr = self.expr_single()?;
                    clauses.push(Clause::For { var, ty, pos, expr });
                    if !self.eat(",") {
                        break;
                    }
                }
            } else if self.kw_then("let", '$') {
                self.eat_kw("let");
                loop {
                    let var = self.var_name()?;
                    let ty = if self.eat_kw("as") { Some(self.seq_type()?) } else { None };
                    self.expect(":=")?;
                    let expr = self.expr_single()?;
                    clauses.push(Clause::Let { var, ty, expr });
                    if !self.eat(",") {
                        break;
                    }
                }
            } else if self.eat_kw("where") {
                clauses.push(Clause::Where(self.expr_single()?));
            } else if self.eat_kws(&["order", "by"]) || self.eat_kws(&["stable", "order", "by"]) {
                let mut specs = Vec::new();
                loop {
                    let expr = self.expr_single()?;
                    let desc = if self.eat_kw("descending") {
                        true
                    } else {
                        self.eat_kw("ascending");
                        false
                    };
                    let mut empty_greatest = self.module.empty_greatest;
                    if self.eat_kws(&["empty", "greatest"]) {
                        empty_greatest = true;
                    } else if self.eat_kws(&["empty", "least"]) {
                        empty_greatest = false;
                    }
                    if self.eat_kw("collation") {
                        self.string_lit()?;
                    }
                    specs.push(OrderSpec { expr, desc, empty_greatest });
                    if !self.eat(",") {
                        break;
                    }
                }
                clauses.push(Clause::OrderBy { specs });
            } else if self.kw_then("count", '$') {
                self.eat_kw("count");
                clauses.push(Clause::Count(self.var_name()?));
            } else if self.eat_kws(&["group", "by"]) {
                let mut keys = Vec::new();
                loop {
                    let v = self.var_name()?;
                    let e = if self.eat(":=") { Some(self.expr_single()?) } else { None };
                    keys.push((v, e));
                    if !self.eat(",") {
                        break;
                    }
                }
                clauses.push(Clause::GroupBy(keys));
            } else if self.eat_kw("return") {
                let ret = self.expr_single()?;
                return Ok(Expr::Flwor { clauses, ret: Box::new(ret) });
            } else {
                return self.err("Expected 'return' in FLWOR expression");
            }
        }
    }

    fn typeswitch(&mut self) -> R<Expr> {
        self.eat_kw("typeswitch");
        self.expect("(")?;
        let operand = self.expr()?;
        self.expect(")")?;
        let mut cases = Vec::new();
        while self.eat_kw("case") {
            let var = if self.at("$") {
                let v = self.var_name()?;
                if !self.eat_kw("as") {
                    return self.err("Expected 'as'");
                }
                Some(v)
            } else {
                None
            };
            let ty = self.seq_type()?;
            if !self.eat_kw("return") {
                return self.err("Expected 'return'");
            }
            let ret = self.expr_single()?;
            cases.push(Case { var, ty, ret });
        }
        if cases.is_empty() || !self.eat_kw("default") {
            return self.err("Expected 'case' / 'default' in typeswitch");
        }
        let default_var = if self.at("$") { Some(self.var_name()?) } else { None };
        if !self.eat_kw("return") {
            return self.err("Expected 'return'");
        }
        let default = self.expr_single()?;
        Ok(Expr::Typeswitch { operand: Box::new(operand), cases, default_var, default: Box::new(default) })
    }

    fn or_expr(&mut self) -> R<Expr> {
        let mut l = self.and_expr()?;
        while self.eat_kw("or") {
            let r = self.and_expr()?;
            l = Expr::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn and_expr(&mut self) -> R<Expr> {
        let mut l = self.comparison()?;
        while self.eat_kw("and") {
            let r = self.comparison()?;
            l = Expr::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn comparison(&mut self) -> R<Expr> {
        let l = self.string_concat()?;
        self.ws();
        let gen = [("<=", Cmp::Le), (">=", Cmp::Ge), ("!=", Cmp::Ne), ("=", Cmp::Eq)];
        if self.starts("<<") {
            self.i += 2;
            let r = self.string_concat()?;
            return Ok(Expr::NodeCmp(NodeCmp::Before, Box::new(l), Box::new(r)));
        }
        if self.starts(">>") {
            self.i += 2;
            let r = self.string_concat()?;
            return Ok(Expr::NodeCmp(NodeCmp::After, Box::new(l), Box::new(r)));
        }
        for (t, op) in gen {
            if self.starts(t) {
                self.i += t.len();
                let r = self.string_concat()?;
                return Ok(Expr::GenCmp(op, Box::new(l), Box::new(r)));
            }
        }
        if self.starts("<") {
            self.i += 1;
            let r = self.string_concat()?;
            return Ok(Expr::GenCmp(Cmp::Lt, Box::new(l), Box::new(r)));
        }
        if self.starts(">") {
            self.i += 1;
            let r = self.string_concat()?;
            return Ok(Expr::GenCmp(Cmp::Gt, Box::new(l), Box::new(r)));
        }
        for (k, op) in [("eq", Cmp::Eq), ("ne", Cmp::Ne), ("lt", Cmp::Lt), ("le", Cmp::Le), ("gt", Cmp::Gt), ("ge", Cmp::Ge)] {
            if self.eat_kw(k) {
                let r = self.string_concat()?;
                return Ok(Expr::ValCmp(op, Box::new(l), Box::new(r)));
            }
        }
        if self.eat_kw("is") {
            let r = self.string_concat()?;
            return Ok(Expr::NodeCmp(NodeCmp::Is, Box::new(l), Box::new(r)));
        }
        Ok(l)
    }

    fn string_concat(&mut self) -> R<Expr> {
        let mut l = self.range()?;
        while self.eat("||") {
            let r = self.range()?;
            l = Expr::Concat(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn range(&mut self) -> R<Expr> {
        let l = self.additive()?;
        if self.eat_kw("to") {
            let r = self.additive()?;
            return Ok(Expr::Range(Box::new(l), Box::new(r)));
        }
        Ok(l)
    }

    fn additive(&mut self) -> R<Expr> {
        let mut l = self.multiplicative()?;
        loop {
            if self.eat("+") {
                let r = self.multiplicative()?;
                l = Expr::Arith(Arith::Add, Box::new(l), Box::new(r));
            } else if self.at("-") {
                self.i += 1;
                let r = self.multiplicative()?;
                l = Expr::Arith(Arith::Sub, Box::new(l), Box::new(r));
            } else {
                return Ok(l);
            }
        }
    }

    fn multiplicative(&mut self) -> R<Expr> {
        let mut l = self.union()?;
        loop {
            let op = if self.eat("*") {
                Arith::Mul
            } else if self.eat_kw("div") {
                Arith::Div
            } else if self.eat_kw("idiv") {
                Arith::IDiv
            } else if self.eat_kw("mod") {
                Arith::Mod
            } else {
                return Ok(l);
            };
            let r = self.union()?;
            l = Expr::Arith(op, Box::new(l), Box::new(r));
        }
    }

    fn union(&mut self) -> R<Expr> {
        let mut l = self.intersect()?;
        loop {
            if self.eat_kw("union") || (self.at("|") && !self.starts("||") && self.eat("|")) {
                let r = self.intersect()?;
                l = Expr::Set(SetOp::Union, Box::new(l), Box::new(r));
            } else {
                return Ok(l);
            }
        }
    }

    fn intersect(&mut self) -> R<Expr> {
        let mut l = self.instance_of()?;
        loop {
            let op = if self.eat_kw("intersect") {
                SetOp::Intersect
            } else if self.eat_kw("except") {
                SetOp::Except
            } else {
                return Ok(l);
            };
            let r = self.instance_of()?;
            l = Expr::Set(op, Box::new(l), Box::new(r));
        }
    }

    fn instance_of(&mut self) -> R<Expr> {
        let l = self.treat()?;
        if self.eat_kws(&["instance", "of"]) {
            let t = self.seq_type()?;
            return Ok(Expr::InstanceOf(Box::new(l), t));
        }
        Ok(l)
    }

    fn treat(&mut self) -> R<Expr> {
        let l = self.castable()?;
        if self.eat_kws(&["treat", "as"]) {
            let t = self.seq_type()?;
            return Ok(Expr::Treat(Box::new(l), t));
        }
        Ok(l)
    }

    fn castable(&mut self) -> R<Expr> {
        let l = self.cast()?;
        if self.eat_kws(&["castable", "as"]) {
            let (t, o) = self.single_type()?;
            return Ok(Expr::Castable(Box::new(l), t, o));
        }
        Ok(l)
    }

    fn cast(&mut self) -> R<Expr> {
        let l = self.unary()?;
        if self.eat_kws(&["cast", "as"]) {
            let (t, o) = self.single_type()?;
            return Ok(Expr::Cast(Box::new(l), t, o));
        }
        Ok(l)
    }

    fn unary(&mut self) -> R<Expr> {
        if self.eat("-") {
            let e = self.unary()?;
            return Ok(match e {
                Expr::Lit(Atomic::Int(i)) => Expr::Lit(Atomic::Int(-i)),
                other => Expr::Neg(Box::new(other)),
            });
        }
        if self.eat("+") {
            let e = self.unary()?;
            return Ok(Expr::Plus(Box::new(e)));
        }
        self.simple_map()
    }

    fn simple_map(&mut self) -> R<Expr> {
        let mut l = self.path()?;
        while self.at("!") && self.peek_at(1) != Some('=') {
            self.i += 1;
            let r = self.path()?;
            l = Expr::SimpleMap(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn can_start_step(&mut self) -> bool {
        self.ws();
        match self.peek() {
            Some(c) if is_name_start(c) => true,
            Some('*' | '@' | '.' | '$' | '(' | '"' | '\'') => true,
            Some(c) if c.is_ascii_digit() => true,
            Some('<') => self.peek_at(1).is_some_and(|c| is_name_start(c) || c == '!' || c == '?'),
            _ => false,
        }
    }

    fn path(&mut self) -> R<Expr> {
        self.ws();
        let (start, mut steps) = if self.starts("//") {
            self.i += 2;
            (PathStart::RootDesc, vec![(false, self.step()?)])
        } else if self.starts("/") {
            self.i += 1;
            if self.can_start_step() {
                (PathStart::Root, vec![(false, self.step()?)])
            } else {
                return Ok(Expr::Path { start: PathStart::Root, steps: Vec::new() });
            }
        } else {
            let first = self.step()?;
            if !self.at("/") {
                return Ok(match first {
                    Step::Filter(e) => e,
                    s => Expr::Path { start: PathStart::Relative, steps: vec![(false, s)] },
                });
            }
            (PathStart::Relative, vec![(false, first)])
        };
        loop {
            if self.at("//") {
                self.i += 2;
                steps.push((true, self.step()?));
            } else if self.at("/") {
                self.i += 1;
                steps.push((false, self.step()?));
            } else {
                break;
            }
        }
        Ok(Expr::Path { start, steps })
    }

    fn predicates(&mut self) -> R<Vec<Expr>> {
        let mut v = Vec::new();
        while self.eat("[") {
            v.push(self.expr()?);
            self.expect("]")?;
        }
        Ok(v)
    }

    fn is_computed_ctor(&mut self) -> bool {
        let save = self.i;
        let r = (|| {
            self.ws();
            let Some(kw) = self.ncname() else { return false };
            match kw.as_str() {
                "text" | "comment" | "document" | "ordered" | "unordered" => {
                    self.ws();
                    self.peek() == Some('{')
                }
                "element" | "attribute" | "processing-instruction" => {
                    self.ws();
                    if self.peek() == Some('{') {
                        return true;
                    }
                    if self.qname_raw().is_some() {
                        self.ws();
                        return self.peek() == Some('{');
                    }
                    false
                }
                _ => false,
            }
        })();
        self.i = save;
        r
    }

    fn step(&mut self) -> R<Step> {
        self.ws();
        if self.eat("@") {
            let test = self.node_test(Axis::Attribute)?;
            let preds = self.predicates()?;
            return Ok(Step::Axis { axis: Axis::Attribute, test, preds });
        }
        if self.starts("..") {
            self.i += 2;
            let preds = self.predicates()?;
            return Ok(Step::Axis { axis: Axis::Parent, test: StepTest::Name(NodeTest::Node), preds });
        }
        let c = self.peek();
        if c == Some('*') {
            let test = self.node_test(Axis::Child)?;
            let preds = self.predicates()?;
            return Ok(Step::Axis { axis: Axis::Child, test, preds });
        }
        if c.is_some_and(is_name_start) && !self.is_computed_ctor() {
            let save = self.i;
            let name = self.ncname().unwrap_or_default();
            if self.at("::") {
                if let Some(axis) = Axis::from_name(&name) {
                    self.i += 2;
                    let test = self.node_test(axis)?;
                    let preds = self.predicates()?;
                    return Ok(Step::Axis { axis, test, preds });
                }
                return self.err(format!("Unknown axis {}", name));
            }
            self.i = save;
            let q = self.qname_raw();
            let is_wild = self.starts(":*");
            let after = self.i;
            self.ws();
            let paren = self.peek() == Some('(');
            self.i = after;
            let local = q.as_ref().map(|q| q.1.clone()).unwrap_or_default();
            let prefixed = q.as_ref().is_some_and(|q| !q.0.is_empty());
            if is_wild || !paren || (!prefixed && KIND_TESTS.contains(&local.as_str())) {
                self.i = save;
                let test = self.node_test(Axis::Child)?;
                let preds = self.predicates()?;
                return Ok(Step::Axis { axis: Axis::Child, test, preds });
            }
            self.i = save;
        }
        let prim = self.primary()?;
        let preds = self.predicates()?;
        Ok(Step::Filter(if preds.is_empty() { prim } else { Expr::Filter(Box::new(prim), preds) }))
    }

    fn node_test(&mut self, axis: Axis) -> R<StepTest> {
        self.ws();
        if self.starts("*:") && self.peek_at(2).is_some_and(is_name_start) {
            self.i += 2;
            let l = self.ncname().unwrap_or_default();
            return Ok(StepTest::LocalWildcard(l));
        }
        if self.eat("*") {
            return Ok(StepTest::Name(NodeTest::AnyName));
        }
        let save = self.i;
        let Some(first) = self.ncname() else { return self.err("Expected a node test") };
        if self.starts(":*") {
            self.i += 2;
            return Ok(StepTest::Name(NodeTest::NsWildcard(self.lookup_prefix(&first)?)));
        }
        self.i = save;
        let (p, l) = self.qname_raw().unwrap();
        if p.is_empty() && KIND_TESTS.contains(&l.as_str()) {
            let after = self.i;
            self.ws();
            if self.peek() == Some('(') {
                self.i = save;
                let k = self.kind_test()?;
                return Ok(match k {
                    ItemType::AnyNode => StepTest::Name(NodeTest::Node),
                    ItemType::Text => StepTest::Name(NodeTest::Text),
                    ItemType::Comment => StepTest::Name(NodeTest::Comment),
                    ItemType::Pi(t) => StepTest::Name(NodeTest::Pi(t)),
                    other => StepTest::Kind(SeqType { item: other, occ: Occ::One }),
                });
            }
            self.i = after;
        }
        let q = if axis == Axis::Attribute || axis == Axis::Namespace { self.resolve_plain(&p, &l)? } else { self.resolve_elem(&p, &l)? };
        Ok(StepTest::Name(NodeTest::Name(q.uri, q.local)))
    }

    fn primary(&mut self) -> R<Expr> {
        self.ws();
        let Some(c) = self.peek() else { return self.err("Unexpected end of query") };
        match c {
            '"' | '\'' => Ok(Expr::Lit(Atomic::Str(self.string_lit()?))),
            '$' => Ok(Expr::Var(self.var_name()?)),
            '(' => {
                self.i += 1;
                if self.eat(")") {
                    return Ok(Expr::Seq(Vec::new()));
                }
                let e = self.expr()?;
                self.expect(")")?;
                Ok(match e {
                    Expr::Seq(v) => Expr::Seq(v),
                    other => Expr::Seq(vec![other]),
                })
            }
            '.' if !self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) => {
                self.i += 1;
                Ok(Expr::ContextItem)
            }
            '<' => self.direct_ctor(),
            c if c.is_ascii_digit() || c == '.' => self.number(),
            c if is_name_start(c) => {
                if self.is_computed_ctor() {
                    return self.computed_ctor();
                }
                let (p, l) = self.expect_qname()?;
                self.ws();
                if self.peek() != Some('(') {
                    return self.err(format!("Unexpected name {}", l));
                }
                if p.is_empty() && (RESERVED_FUNCTIONS.contains(&l.as_str()) || KIND_TESTS.contains(&l.as_str())) {
                    return self.err(format!("'{}' cannot be used as a function name", l));
                }
                self.i += 1;
                let mut args = Vec::new();
                if !self.eat(")") {
                    loop {
                        args.push(self.expr_single()?);
                        if self.eat(")") {
                            break;
                        }
                        self.expect(",")?;
                    }
                }
                let name = self.resolve_fn(&p, &l)?;
                Ok(Expr::Call { name, args })
            }
            _ => self.err("Unexpected character"),
        }
    }

    fn number(&mut self) -> R<Expr> {
        let st = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        let mut dec = false;
        if self.peek() == Some('.') && self.peek_at(1) != Some('.') {
            dec = true;
            self.i += 1;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        let mut dbl = false;
        if matches!(self.peek(), Some('e' | 'E')) && (self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) || (matches!(self.peek_at(1), Some('+' | '-')) && self.peek_at(2).is_some_and(|c| c.is_ascii_digit()))) {
            dbl = true;
            self.i += 2;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        let text: String = self.s[st..self.i].iter().collect();
        if self.peek().is_some_and(is_name_start) {
            return self.err("A numeric literal must not be followed by a name");
        }
        Ok(Expr::Lit(if dbl {
            Atomic::Dbl(text.parse().map_err(|_| XqError::syntax(format!("Invalid number {}", text)))?)
        } else if dec {
            Atomic::Dec(Decimal::parse(&text).ok_or_else(|| XqError::syntax(format!("Invalid number {}", text)))?)
        } else {
            match text.parse::<i64>() {
                Ok(i) => Atomic::Int(i),
                Err(_) => Atomic::Dec(Decimal::parse(&text).unwrap_or_default()),
            }
        }))
    }

    fn ctor_name(&mut self, elem: bool) -> R<CtorName> {
        self.ws();
        if self.peek() == Some('{') {
            let e = self.enclosed()?;
            return Ok(CtorName::Computed(Box::new(e)));
        }
        let (p, l) = self.expect_qname()?;
        let q = if elem { self.resolve_elem(&p, &l)? } else { self.resolve_plain(&p, &l)? };
        Ok(CtorName::Static(q, p))
    }

    fn computed_ctor(&mut self) -> R<Expr> {
        self.ws();
        let kw = self.ncname().unwrap_or_default();
        match kw.as_str() {
            "element" => {
                let name = self.ctor_name(true)?;
                let content = self.enclosed()?;
                Ok(Expr::Elem { name, ns: Vec::new(), attrs: Vec::new(), content: vec![content] })
            }
            "attribute" => {
                let name = self.ctor_name(false)?;
                let value = self.enclosed()?;
                Ok(Expr::Attr { name, value: Box::new(value) })
            }
            "processing-instruction" => {
                self.ws();
                let target = if self.peek() == Some('{') {
                    CtorName::Computed(Box::new(self.enclosed()?))
                } else {
                    let n = self.ncname().unwrap_or_default();
                    CtorName::Static(QN::local(n), String::new())
                };
                let value = self.enclosed()?;
                Ok(Expr::Pi { target, value: Box::new(value) })
            }
            "text" => Ok(Expr::Text(Box::new(self.enclosed()?))),
            "comment" => Ok(Expr::Comment(Box::new(self.enclosed()?))),
            "document" => Ok(Expr::Document(Box::new(self.enclosed()?))),
            _ => self.enclosed(), // ordered / unordered
        }
    }

    // ---------------------------------------------------------------- direct constructors

    fn direct_ctor(&mut self) -> R<Expr> {
        if self.starts("<!--") {
            self.i += 4;
            let st = self.i;
            while self.i < self.s.len() && !self.starts("-->") {
                self.i += 1;
            }
            if !self.starts("-->") {
                return self.err("Unterminated comment constructor");
            }
            let text: String = self.s[st..self.i].iter().collect();
            self.i += 3;
            return Ok(Expr::Comment(Box::new(Expr::Lit(Atomic::Str(text)))));
        }
        if self.starts("<?") {
            self.i += 2;
            let target = self.ncname().unwrap_or_default();
            while self.peek().is_some_and(|c| c.is_whitespace()) {
                self.i += 1;
            }
            let st = self.i;
            while self.i < self.s.len() && !self.starts("?>") {
                self.i += 1;
            }
            if !self.starts("?>") {
                return self.err("Unterminated processing instruction constructor");
            }
            let text: String = self.s[st..self.i].iter().collect();
            self.i += 2;
            return Ok(Expr::Pi { target: CtorName::Static(QN::local(target), String::new()), value: Box::new(Expr::Lit(Atomic::Str(text))) });
        }
        self.i += 1; // '<'
        let Some((ep, el)) = self.qname_raw() else { return self.err("Expected an element name") };
        // attributes (raw pass)
        let mut raw_attrs: Vec<(String, String, usize)> = Vec::new();
        let mut ns_decls: Vec<(String, String)> = Vec::new();
        loop {
            let had_ws = self.peek().is_some_and(|c| c.is_whitespace());
            while self.peek().is_some_and(|c| c.is_whitespace()) {
                self.i += 1;
            }
            if self.starts("/>") || self.starts(">") {
                break;
            }
            if !had_ws {
                return self.err("Expected whitespace before an attribute");
            }
            let Some((ap, al)) = self.qname_raw() else { return self.err("Expected an attribute name") };
            while self.peek().is_some_and(|c| c.is_whitespace()) {
                self.i += 1;
            }
            if self.peek() != Some('=') {
                return self.err("Expected '=' after attribute name");
            }
            self.i += 1;
            while self.peek().is_some_and(|c| c.is_whitespace()) {
                self.i += 1;
            }
            let start = self.i;
            if ap.is_empty() && al == "xmlns" || ap == "xmlns" {
                let parts = self.attr_value()?;
                let mut uri = String::new();
                for p in parts {
                    match p {
                        AttrPart::Lit(s) => uri.push_str(&s),
                        AttrPart::Expr(_) => return Err(XqError::static_err("XQST0022", "A namespace declaration attribute must have a literal value")),
                    }
                }
                ns_decls.push((if ap.is_empty() { String::new() } else { al.clone() }, uri));
            } else {
                // skip for now; parsed again once namespaces are in scope
                self.attr_value()?;
                raw_attrs.push((ap, al, start));
            }
        }
        let end_pos = self.i;
        // namespace scope
        let saved_default = self.default_elem_ns.clone();
        let ns_len = self.ns.len();
        for (p, u) in &ns_decls {
            if p.is_empty() {
                self.default_elem_ns = u.clone();
            } else {
                self.ns.push((p.clone(), u.clone()));
            }
        }
        let result = (|| {
            let name = self.resolve_elem(&ep, &el)?;
            let mut attrs = Vec::new();
            for (ap, al, start) in &raw_attrs {
                self.i = *start;
                let parts = self.attr_value()?;
                let q = self.resolve_plain(ap, al)?;
                if attrs.iter().any(|(a, _, _): &(QN, String, Vec<AttrPart>)| *a == q) {
                    return Err(XqError::static_err("XQST0040", format!("Duplicate attribute {}", al)));
                }
                attrs.push((q, ap.clone(), parts));
            }
            self.i = end_pos;
            let mut content = Vec::new();
            if self.starts("/>") {
                self.i += 2;
            } else {
                self.i += 1; // '>'
                content = self.element_content(&ep, &el)?;
            }
            Ok(Expr::Elem { name: CtorName::Static(name, ep.clone()), ns: ns_decls.clone(), attrs, content })
        })();
        self.ns.truncate(ns_len);
        self.default_elem_ns = saved_default;
        result
    }

    /// An attribute value (at the quote) as literal and enclosed-expression parts.
    fn attr_value(&mut self) -> R<Vec<AttrPart>> {
        let q = match self.peek() {
            Some(c @ ('"' | '\'')) => c,
            _ => return self.err("Expected a quoted attribute value"),
        };
        self.i += 1;
        let mut parts = Vec::new();
        let mut lit = String::new();
        loop {
            match self.peek() {
                None => return self.err("Unterminated attribute value"),
                Some(c) if c == q => {
                    if self.peek_at(1) == Some(q) {
                        lit.push(q);
                        self.i += 2;
                    } else {
                        self.i += 1;
                        break;
                    }
                }
                Some('{') if self.peek_at(1) == Some('{') => {
                    lit.push('{');
                    self.i += 2;
                }
                Some('}') if self.peek_at(1) == Some('}') => {
                    lit.push('}');
                    self.i += 2;
                }
                Some('{') => {
                    if !lit.is_empty() {
                        parts.push(AttrPart::Lit(std::mem::take(&mut lit)));
                    }
                    let e = self.enclosed()?;
                    parts.push(AttrPart::Expr(e));
                }
                Some('}') => return self.err("Unescaped '}' in attribute value"),
                Some('<') => return self.err("'<' is not allowed in an attribute value"),
                Some('&') => lit.push_str(&self.entity_ref()?),
                Some(c) => {
                    // attribute value normalization of whitespace characters
                    lit.push(if matches!(c, '\t' | '\n' | '\r') { ' ' } else { c });
                    self.i += 1;
                }
            }
        }
        if !lit.is_empty() {
            parts.push(AttrPart::Lit(lit));
        }
        Ok(parts)
    }

    fn element_content(&mut self, ep: &str, el: &str) -> R<Vec<Expr>> {
        let mut content = Vec::new();
        let mut text = String::new();
        let mut boundary = true; // text so far is boundary whitespace candidate
        let flush = |content: &mut Vec<Expr>, text: &mut String, boundary: &mut bool, preserve: bool| {
            if !text.is_empty() && (preserve || !*boundary || !text.chars().all(char::is_whitespace)) {
                content.push(Expr::TextLit(std::mem::take(text)));
            }
            text.clear();
            *boundary = true;
        };
        loop {
            if self.i >= self.s.len() {
                return self.err(format!("Unterminated element <{}>", el));
            }
            if self.starts("</") {
                flush(&mut content, &mut text, &mut boundary, self.boundary_preserve);
                self.i += 2;
                let Some((p, l)) = self.qname_raw() else { return self.err("Expected an end tag name") };
                if p != ep || l != el {
                    return self.err(format!("End tag </{}> does not match <{}>", if p.is_empty() { l } else { format!("{}:{}", p, l) }, el));
                }
                while self.peek().is_some_and(|c| c.is_whitespace()) {
                    self.i += 1;
                }
                if self.peek() != Some('>') {
                    return self.err("Expected '>'");
                }
                self.i += 1;
                return Ok(content);
            }
            if self.starts("<![CDATA[") {
                self.i += 9;
                let st = self.i;
                while self.i < self.s.len() && !self.starts("]]>") {
                    self.i += 1;
                }
                let t: String = self.s[st..self.i].iter().collect();
                self.i += 3;
                text.push_str(&t);
                boundary = false;
                continue;
            }
            if self.starts("<") {
                flush(&mut content, &mut text, &mut boundary, self.boundary_preserve);
                content.push(self.direct_ctor()?);
                continue;
            }
            if self.starts("{{") {
                text.push('{');
                boundary = false;
                self.i += 2;
                continue;
            }
            if self.starts("}}") {
                text.push('}');
                boundary = false;
                self.i += 2;
                continue;
            }
            match self.peek().unwrap() {
                '{' => {
                    flush(&mut content, &mut text, &mut boundary, self.boundary_preserve);
                    content.push(self.enclosed()?);
                }
                '}' => return self.err("Unescaped '}' in element content"),
                '&' => {
                    text.push_str(&self.entity_ref()?);
                    boundary = false;
                }
                c => {
                    if !c.is_whitespace() {
                        boundary = false;
                    }
                    text.push(c);
                    self.i += 1;
                }
            }
        }
    }
}
