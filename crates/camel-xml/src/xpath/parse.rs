//! XPath 1.0 lexer (with the disambiguation rules of section 3.7) and parser.

use super::ast::*;
use super::XPathError;
use crate::parser::{is_name_char, is_name_start_char};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    LParen,
    RParen,
    LBracket,
    RBracket,
    Dot,
    DotDot,
    At,
    Comma,
    ColonColon,
    /// `*`, `prefix:*` or a QName in a name-test position.
    NameTest(Option<String>, String),
    NodeType(String),
    /// Operator (`and or mod div / // | + - = != < <= > >= *`).
    Op(&'static str),
    FuncName(Option<String>, String),
    Axis(String),
    Literal(String),
    Number(f64),
    Var(Option<String>, String),
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

fn ncname(c: &[char], i: &mut usize) -> Option<String> {
    if *i < c.len() && is_name_start_char(c[*i]) && c[*i] != ':' {
        let st = *i;
        while *i < c.len() && is_name_char(c[*i]) && c[*i] != ':' {
            *i += 1;
        }
        Some(c[st..*i].iter().collect())
    } else {
        None
    }
}

pub(crate) fn tokenize(expr: &str) -> Result<Vec<Tok>, XPathError> {
    tokenize_v(expr, false)
}

/// XPath 2.0 keywords recognized in operator position.
const V2_KEYWORDS: &[&str] = &["then", "else", "return", "satisfies", "in", "to", "eq", "ne", "lt", "le", "gt", "ge", "intersect", "except", "union", "idiv"];

/// Tokenize; `v2` adds the XPath 2.0 keywords (`then`, `return`, `eq`, `to`...) and `||`.
pub(crate) fn tokenize_v(expr: &str, v2: bool) -> Result<Vec<Tok>, XPathError> {
    let c: Vec<char> = expr.chars().collect();
    let mut toks: Vec<Tok> = Vec::new();
    let mut i = 0;
    let err = |m: String| XPathError::syntax(expr, m);
    while i < c.len() {
        let ch = c[i];
        if is_ws(ch) {
            i += 1;
            continue;
        }
        // operator context per XPath 1.0 section 3.7
        let operator_ctx = match toks.last() {
            None => false,
            Some(Tok::At | Tok::ColonColon | Tok::LParen | Tok::LBracket | Tok::Comma | Tok::Op(_)) => false,
            _ => true,
        };
        match ch {
            '(' => {
                toks.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                toks.push(Tok::RParen);
                i += 1;
            }
            '[' => {
                toks.push(Tok::LBracket);
                i += 1;
            }
            ']' => {
                toks.push(Tok::RBracket);
                i += 1;
            }
            '@' => {
                toks.push(Tok::At);
                i += 1;
            }
            ',' => {
                toks.push(Tok::Comma);
                i += 1;
            }
            ':' if c.get(i + 1) == Some(&':') => {
                toks.push(Tok::ColonColon);
                i += 2;
            }
            '.' if c.get(i + 1) == Some(&'.') => {
                toks.push(Tok::DotDot);
                i += 2;
            }
            '.' if !c.get(i + 1).map(|d| d.is_ascii_digit()).unwrap_or(false) => {
                toks.push(Tok::Dot);
                i += 1;
            }
            '0'..='9' | '.' => {
                let st = i;
                while i < c.len() && c[i].is_ascii_digit() {
                    i += 1;
                }
                if i < c.len() && c[i] == '.' {
                    i += 1;
                    while i < c.len() && c[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let s: String = c[st..i].iter().collect();
                toks.push(Tok::Number(s.parse::<f64>().map_err(|_| err(format!("invalid number {}", s)))?));
            }
            '"' | '\'' => {
                let st = i + 1;
                let mut j = st;
                while j < c.len() && c[j] != ch {
                    j += 1;
                }
                if j >= c.len() {
                    return Err(err("misquoted literal... expected double quote!".into()));
                }
                toks.push(Tok::Literal(c[st..j].iter().collect()));
                i = j + 1;
            }
            '/' => {
                if c.get(i + 1) == Some(&'/') {
                    toks.push(Tok::Op("//"));
                    i += 2;
                } else {
                    toks.push(Tok::Op("/"));
                    i += 1;
                }
            }
            '|' if v2 && c.get(i + 1) == Some(&'|') => {
                toks.push(Tok::Op("||"));
                i += 2;
            }
            '|' => {
                toks.push(Tok::Op("|"));
                i += 1;
            }
            '+' => {
                toks.push(Tok::Op("+"));
                i += 1;
            }
            '-' => {
                toks.push(Tok::Op("-"));
                i += 1;
            }
            '=' => {
                toks.push(Tok::Op("="));
                i += 1;
            }
            '!' if c.get(i + 1) == Some(&'=') => {
                toks.push(Tok::Op("!="));
                i += 2;
            }
            '<' => {
                if c.get(i + 1) == Some(&'=') {
                    toks.push(Tok::Op("<="));
                    i += 2;
                } else {
                    toks.push(Tok::Op("<"));
                    i += 1;
                }
            }
            '>' => {
                if c.get(i + 1) == Some(&'=') {
                    toks.push(Tok::Op(">="));
                    i += 2;
                } else {
                    toks.push(Tok::Op(">"));
                    i += 1;
                }
            }
            '*' => {
                i += 1;
                if operator_ctx {
                    toks.push(Tok::Op("*"));
                } else {
                    toks.push(Tok::NameTest(None, "*".into()));
                }
            }
            '$' => {
                i += 1;
                let a = ncname(&c, &mut i).ok_or_else(|| err("expected a variable name after '$'".into()))?;
                if i + 1 < c.len() && c[i] == ':' && c[i + 1] != ':' {
                    i += 1;
                    let b = ncname(&c, &mut i).ok_or_else(|| err("invalid variable name".into()))?;
                    toks.push(Tok::Var(Some(a), b));
                } else {
                    toks.push(Tok::Var(None, a));
                }
            }
            _ if is_name_start_char(ch) && ch != ':' => {
                let a = ncname(&c, &mut i).unwrap();
                if operator_ctx {
                    match a.as_str() {
                        "and" => toks.push(Tok::Op("and")),
                        "or" => toks.push(Tok::Op("or")),
                        "mod" => toks.push(Tok::Op("mod")),
                        "div" => toks.push(Tok::Op("div")),
                        k if v2 && V2_KEYWORDS.contains(&k) => toks.push(Tok::Op(V2_KEYWORDS.iter().find(|x| **x == k).unwrap())),
                        _ => return Err(err(format!("unexpected name '{}' (an operator was expected)", a))),
                    }
                    continue;
                }
                // QName / prefix:* ?
                let (prefix, local) = if i + 1 < c.len() && c[i] == ':' && c[i + 1] != ':' {
                    if c[i + 1] == '*' {
                        i += 2;
                        toks.push(Tok::NameTest(Some(a), "*".into()));
                        continue;
                    }
                    i += 1;
                    let b = ncname(&c, &mut i).ok_or_else(|| err(format!("invalid QName after '{}:'", a)))?;
                    (Some(a), b)
                } else {
                    (None, a)
                };
                // lookahead
                let mut j = i;
                while j < c.len() && is_ws(c[j]) {
                    j += 1;
                }
                if c.get(j) == Some(&'(') {
                    if prefix.is_none() && matches!(local.as_str(), "comment" | "text" | "processing-instruction" | "node") {
                        toks.push(Tok::NodeType(local));
                    } else {
                        toks.push(Tok::FuncName(prefix, local));
                    }
                } else if c.get(j) == Some(&':') && c.get(j + 1) == Some(&':') && prefix.is_none() {
                    toks.push(Tok::Axis(local));
                } else {
                    toks.push(Tok::NameTest(prefix, local));
                }
            }
            _ => return Err(err(format!("unexpected character '{}'", ch))),
        }
    }
    Ok(toks)
}

/// Resolves namespace prefixes at compile time.
pub type PrefixResolver<'a> = &'a dyn Fn(&str) -> Option<String>;

pub(crate) struct Parser<'a> {
    pub toks: Vec<Tok>,
    pub i: usize,
    pub text: &'a str,
    pub ns: PrefixResolver<'a>,
    /// XPath 2.0 syntax (`if`, `for`, `some`/`every`, sequences, value comparisons, `to`...).
    pub v2: bool,
}

impl<'a> Parser<'a> {
    fn err<T>(&self, m: impl Into<String>) -> Result<T, XPathError> {
        Err(XPathError::syntax(self.text, m.into()))
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i)
    }

    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if *o == op)
    }

    fn expect(&mut self, t: Tok, what: &str) -> Result<(), XPathError> {
        if self.peek() == Some(&t) {
            self.i += 1;
            Ok(())
        } else {
            self.err(format!("expected {} but found {}", what, self.describe()))
        }
    }

    fn describe(&self) -> String {
        match self.peek() {
            None => "end of expression".into(),
            Some(t) => format!("{:?}", t),
        }
    }

    fn resolve(&self, prefix: &Option<String>) -> Result<String, XPathError> {
        match prefix {
            None => Ok(String::new()),
            Some(p) => match (self.ns)(p) {
                Some(u) => Ok(u),
                None => self.err(format!("Prefix must resolve to a namespace: {}", p)),
            },
        }
    }

    pub fn parse_all(&mut self) -> Result<Expr, XPathError> {
        if self.toks.is_empty() {
            return self.err("empty expression");
        }
        let e = if self.v2 { self.expr_seq()? } else { self.or()? };
        if self.i < self.toks.len() {
            return self.err(format!("unexpected token {}", self.describe()));
        }
        Ok(e)
    }

    /// `ExprSingle (, ExprSingle)*` (XPath 2.0).
    fn expr_seq(&mut self) -> Result<Expr, XPathError> {
        let first = self.expr_single()?;
        if self.peek() != Some(&Tok::Comma) {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.peek() == Some(&Tok::Comma) {
            self.i += 1;
            items.push(self.expr_single()?);
        }
        Ok(Expr::Sequence(items))
    }

    /// `if`, `for`, `some` / `every` or an `or` expression.
    fn expr_single(&mut self) -> Result<Expr, XPathError> {
        if !self.v2 {
            return self.or();
        }
        let next_is_var = matches!(self.toks.get(self.i + 1), Some(Tok::Var(..)));
        match self.peek().cloned() {
            Some(Tok::FuncName(None, l)) if l == "if" => {
                self.i += 1;
                self.expect(Tok::LParen, "'('")?;
                let c = self.expr_seq()?;
                self.expect(Tok::RParen, "')'")?;
                self.expect(Tok::Op("then"), "'then'")?;
                let a = self.expr_single()?;
                self.expect(Tok::Op("else"), "'else'")?;
                let b = self.expr_single()?;
                Ok(Expr::If(Box::new(c), Box::new(a), Box::new(b)))
            }
            Some(Tok::NameTest(None, l)) if next_is_var && (l == "for" || l == "some" || l == "every") => {
                self.i += 1;
                let mut binds = Vec::new();
                loop {
                    let (p, v) = match self.peek().cloned() {
                        Some(Tok::Var(p, v)) => (p, v),
                        _ => return self.err(format!("expected a variable but found {}", self.describe())),
                    };
                    self.i += 1;
                    self.expect(Tok::Op("in"), "'in'")?;
                    let e = self.expr_single()?;
                    binds.push((self.resolve(&p)?, v, e));
                    if self.peek() == Some(&Tok::Comma) {
                        self.i += 1;
                    } else {
                        break;
                    }
                }
                if l == "for" {
                    self.expect(Tok::Op("return"), "'return'")?;
                    Ok(Expr::For(binds, Box::new(self.expr_single()?)))
                } else {
                    self.expect(Tok::Op("satisfies"), "'satisfies'")?;
                    Ok(Expr::Quantified(l == "every", binds, Box::new(self.expr_single()?)))
                }
            }
            _ => self.or(),
        }
    }

    fn or(&mut self) -> Result<Expr, XPathError> {
        let mut l = self.and()?;
        while self.is_op("or") {
            self.i += 1;
            l = Expr::Or(Box::new(l), Box::new(self.and()?));
        }
        Ok(l)
    }

    fn and(&mut self) -> Result<Expr, XPathError> {
        let mut l = self.equality()?;
        while self.is_op("and") {
            self.i += 1;
            l = Expr::And(Box::new(l), Box::new(self.equality()?));
        }
        Ok(l)
    }

    fn equality(&mut self) -> Result<Expr, XPathError> {
        let mut l = self.relational()?;
        loop {
            let op = if self.is_op("=") || self.is_op("eq") {
                CmpOp::Eq
            } else if self.is_op("!=") || self.is_op("ne") {
                CmpOp::Ne
            } else {
                return Ok(l);
            };
            self.i += 1;
            l = Expr::Cmp(op, Box::new(l), Box::new(self.relational()?));
        }
    }

    fn relational(&mut self) -> Result<Expr, XPathError> {
        let mut l = self.concat()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op("<" | "lt")) => CmpOp::Lt,
                Some(Tok::Op("<=" | "le")) => CmpOp::Le,
                Some(Tok::Op(">" | "gt")) => CmpOp::Gt,
                Some(Tok::Op(">=" | "ge")) => CmpOp::Ge,
                _ => return Ok(l),
            };
            self.i += 1;
            l = Expr::Cmp(op, Box::new(l), Box::new(self.concat()?));
        }
    }

    /// `a || b` (XPath 3.0 string concatenation) over range expressions.
    fn concat(&mut self) -> Result<Expr, XPathError> {
        let mut l = self.range()?;
        while self.is_op("||") {
            self.i += 1;
            let r = self.range()?;
            l = Expr::Func { uri: String::new(), local: "concat".into(), prefix: None, args: vec![l, r] };
        }
        Ok(l)
    }

    /// `a to b` (XPath 2.0).
    fn range(&mut self) -> Result<Expr, XPathError> {
        let l = self.additive()?;
        if self.is_op("to") {
            self.i += 1;
            return Ok(Expr::Range(Box::new(l), Box::new(self.additive()?)));
        }
        Ok(l)
    }

    fn additive(&mut self) -> Result<Expr, XPathError> {
        let mut l = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op("+")) => ArithOp::Add,
                Some(Tok::Op("-")) => ArithOp::Sub,
                _ => return Ok(l),
            };
            self.i += 1;
            l = Expr::Arith(op, Box::new(l), Box::new(self.multiplicative()?));
        }
    }

    fn multiplicative(&mut self) -> Result<Expr, XPathError> {
        let mut l = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op("*")) => ArithOp::Mul,
                Some(Tok::Op("div")) => ArithOp::Div,
                Some(Tok::Op("mod")) => ArithOp::Mod,
                Some(Tok::Op("idiv")) => {
                    self.i += 1;
                    let r = self.unary()?;
                    l = Expr::Func { uri: String::new(), local: "\u{0}idiv".into(), prefix: None, args: vec![l, r] };
                    continue;
                }
                _ => return Ok(l),
            };
            self.i += 1;
            l = Expr::Arith(op, Box::new(l), Box::new(self.unary()?));
        }
    }

    fn unary(&mut self) -> Result<Expr, XPathError> {
        if self.is_op("-") {
            self.i += 1;
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        self.union()
    }

    fn union(&mut self) -> Result<Expr, XPathError> {
        let mut l = self.path_expr()?;
        loop {
            if self.is_op("|") || self.is_op("union") {
                self.i += 1;
                l = Expr::Union(Box::new(l), Box::new(self.path_expr()?));
            } else if self.is_op("intersect") {
                self.i += 1;
                l = Expr::Intersect(Box::new(l), Box::new(self.path_expr()?));
            } else if self.is_op("except") {
                self.i += 1;
                l = Expr::Except(Box::new(l), Box::new(self.path_expr()?));
            } else {
                return Ok(l);
            }
        }
    }

    fn starts_step(&self) -> bool {
        matches!(self.peek(), Some(Tok::Dot | Tok::DotDot | Tok::At | Tok::Axis(_) | Tok::NameTest(..) | Tok::NodeType(_)))
    }

    fn path_expr(&mut self) -> Result<Expr, XPathError> {
        match self.peek() {
            Some(Tok::Op("/")) => {
                self.i += 1;
                let steps = if self.starts_step() { self.relative_path()? } else { Vec::new() };
                Ok(Expr::Path(PathExpr { start: PathStart::Root, steps }))
            }
            Some(Tok::Op("//")) => {
                self.i += 1;
                let mut steps = vec![Step::descendant_or_self()];
                steps.extend(self.relative_path()?);
                Ok(Expr::Path(PathExpr { start: PathStart::Root, steps }))
            }
            _ if self.starts_step() => {
                let steps = self.relative_path()?;
                Ok(Expr::Path(PathExpr { start: PathStart::Context, steps }))
            }
            _ => {
                let prim = self.primary()?;
                let mut preds = Vec::new();
                while self.peek() == Some(&Tok::LBracket) {
                    preds.push(self.predicate()?);
                }
                let filter = if preds.is_empty() { prim } else { Expr::Filter(Box::new(prim), preds) };
                if self.is_op("/") || self.is_op("//") {
                    let mut steps = Vec::new();
                    if self.is_op("//") {
                        steps.push(Step::descendant_or_self());
                    }
                    self.i += 1;
                    steps.extend(self.relative_path()?);
                    Ok(Expr::Path(PathExpr { start: PathStart::Expr(Box::new(filter)), steps }))
                } else {
                    Ok(filter)
                }
            }
        }
    }

    fn relative_path(&mut self) -> Result<Vec<Step>, XPathError> {
        let mut steps = vec![self.step()?];
        loop {
            if self.is_op("/") {
                self.i += 1;
                steps.push(self.step()?);
            } else if self.is_op("//") {
                self.i += 1;
                steps.push(Step::descendant_or_self());
                steps.push(self.step()?);
            } else {
                return Ok(steps);
            }
        }
    }

    fn step(&mut self) -> Result<Step, XPathError> {
        match self.peek() {
            Some(Tok::Dot) => {
                self.i += 1;
                return Ok(Step { axis: Axis::SelfAxis, test: NodeTest::Node, preds: Vec::new() });
            }
            Some(Tok::DotDot) => {
                self.i += 1;
                return Ok(Step { axis: Axis::Parent, test: NodeTest::Node, preds: Vec::new() });
            }
            _ => {}
        }
        let axis = match self.peek().cloned() {
            Some(Tok::At) => {
                self.i += 1;
                Axis::Attribute
            }
            Some(Tok::Axis(name)) => {
                self.i += 1;
                self.expect(Tok::ColonColon, "'::'")?;
                match Axis::from_name(&name) {
                    Some(a) => a,
                    None => return self.err(format!("unknown axis: {}", name)),
                }
            }
            _ => Axis::Child,
        };
        let test = match self.peek().cloned() {
            Some(Tok::NameTest(p, l)) => {
                self.i += 1;
                if l == "*" {
                    match p {
                        None => NodeTest::AnyName,
                        Some(_) => NodeTest::NsWildcard(self.resolve(&p)?),
                    }
                } else {
                    NodeTest::Name(self.resolve(&p)?, l)
                }
            }
            Some(Tok::NodeType(t)) => {
                self.i += 1;
                self.expect(Tok::LParen, "'('")?;
                let test = match t.as_str() {
                    "node" => NodeTest::Node,
                    "text" => NodeTest::Text,
                    "comment" => NodeTest::Comment,
                    _ => {
                        if let Some(Tok::Literal(s)) = self.peek().cloned() {
                            self.i += 1;
                            NodeTest::Pi(Some(s))
                        } else {
                            NodeTest::Pi(None)
                        }
                    }
                };
                self.expect(Tok::RParen, "')'")?;
                test
            }
            _ => return self.err(format!("expected a node test but found {}", self.describe())),
        };
        let mut preds = Vec::new();
        while self.peek() == Some(&Tok::LBracket) {
            preds.push(self.predicate()?);
        }
        Ok(Step { axis, test, preds })
    }

    fn predicate(&mut self) -> Result<Expr, XPathError> {
        self.expect(Tok::LBracket, "'['")?;
        let e = if self.v2 { self.expr_seq()? } else { self.or()? };
        self.expect(Tok::RBracket, "']'")?;
        Ok(e)
    }

    fn primary(&mut self) -> Result<Expr, XPathError> {
        match self.peek().cloned() {
            Some(Tok::Var(p, l)) => {
                self.i += 1;
                Ok(Expr::Var(self.resolve(&p)?, l))
            }
            Some(Tok::LParen) => {
                self.i += 1;
                if self.v2 && self.peek() == Some(&Tok::RParen) {
                    self.i += 1;
                    return Ok(Expr::Sequence(Vec::new()));
                }
                let e = if self.v2 { self.expr_seq()? } else { self.or()? };
                self.expect(Tok::RParen, "')'")?;
                Ok(e)
            }
            Some(Tok::Literal(s)) => {
                self.i += 1;
                Ok(Expr::Literal(s))
            }
            Some(Tok::Number(n)) => {
                self.i += 1;
                Ok(Expr::Number(n))
            }
            Some(Tok::FuncName(p, l)) => {
                self.i += 1;
                let uri = self.resolve(&p)?;
                self.expect(Tok::LParen, "'('")?;
                let mut args = Vec::new();
                if self.peek() != Some(&Tok::RParen) {
                    loop {
                        args.push(self.expr_single()?);

                        if self.peek() == Some(&Tok::Comma) {
                            self.i += 1;
                        } else {
                            break;
                        }
                    }
                }
                self.expect(Tok::RParen, "')'")?;
                if uri.is_empty() {
                    check_core_arity(&l, args.len()).map_err(|m| XPathError::syntax(self.text, m))?;
                }
                Ok(Expr::Func { uri, local: l, prefix: p, args })
            }
            _ => self.err(format!("unexpected token {}", self.describe())),
        }
    }
}

/// Arity check for core functions (unknown names are checked at evaluation time, so that
/// extension functions provided by an environment, e.g. XSLT, work).
fn check_core_arity(name: &str, n: usize) -> Result<(), String> {
    let (min, max): (usize, usize) = match name {
        "last" | "position" | "true" | "false" => (0, 0),
        "count" | "id" | "boolean" | "not" | "lang" | "sum" | "floor" | "ceiling" | "round" | "upper-case" | "lower-case" => (1, 1),
        "local-name" | "namespace-uri" | "name" | "string" | "string-length" | "normalize-space" | "number" => (0, 1),
        "concat" => (2, usize::MAX),
        "starts-with" | "ends-with" | "contains" | "substring-before" | "substring-after" => (2, 2),
        "substring" => (2, 3),
        "translate" => (3, 3),
        _ => return Ok(()),
    };
    if n < min || n > max {
        return Err(format!("Could not find function: {} with {} argument(s) (wrong number of arguments)", name, n));
    }
    Ok(())
}
