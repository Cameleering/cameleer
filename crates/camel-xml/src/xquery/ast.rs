//! XQuery abstract syntax tree. Names are resolved to expanded names at parse time.

use std::collections::HashMap;

use crate::xpath::ast::{Axis, NodeTest};

use super::types::{Atomic, AtomicType, SeqType};

/// An expanded name (`{uri}local`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct QN {
    pub uri: String,
    pub local: String,
}

impl QN {
    pub fn new(uri: impl Into<String>, local: impl Into<String>) -> QN {
        QN { uri: uri.into(), local: local.into() }
    }
    /// A name without namespace.
    pub fn local(local: impl Into<String>) -> QN {
        QN { uri: String::new(), local: local.into() }
    }
}

impl std::fmt::Display for QN {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.uri.is_empty() {
            f.write_str(&self.local)
        } else {
            write!(f, "Q{{{}}}{}", self.uri, self.local)
        }
    }
}

/// General comparison operators (`=`, `!=`...) and value comparison operators (`eq`...).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Node comparisons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeCmp {
    Is,
    Before,
    After,
}

/// Arithmetic operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arith {
    Add,
    Sub,
    Mul,
    Div,
    IDiv,
    Mod,
}

/// Set operators on node sequences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetOp {
    Union,
    Intersect,
    Except,
}

/// A step test: a name test (reusing the XPath 1.0 node tests) or a kind test.
#[derive(Clone, Debug, PartialEq)]
pub enum StepTest {
    Name(NodeTest),
    /// `*:local`
    LocalWildcard(String),
    Kind(SeqType),
}

/// One step of a path.
#[derive(Clone, Debug)]
pub enum Step {
    Axis { axis: Axis, test: StepTest, preds: Vec<Expr> },
    /// A filter (primary) expression used as a step.
    Filter(Expr),
}

/// Where a path starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathStart {
    Root,
    /// `//`: root then descendant-or-self.
    RootDesc,
    Relative,
}

/// A name in a constructor: static or computed.
#[derive(Clone, Debug)]
pub enum CtorName {
    Static(QN, String),
    Computed(Box<Expr>),
}

/// Part of an attribute value in a direct constructor.
#[derive(Clone, Debug)]
pub enum AttrPart {
    Lit(String),
    Expr(Expr),
}

/// A FLWOR clause.
#[derive(Clone, Debug)]
pub enum Clause {
    For { var: QN, ty: Option<SeqType>, pos: Option<QN>, expr: Expr },
    Let { var: QN, ty: Option<SeqType>, expr: Expr },
    Where(Expr),
    OrderBy { specs: Vec<OrderSpec> },
    Count(QN),
    GroupBy(Vec<(QN, Option<Expr>)>),
}

/// An `order by` key.
#[derive(Clone, Debug)]
pub struct OrderSpec {
    pub expr: Expr,
    pub desc: bool,
    pub empty_greatest: bool,
}

/// A `typeswitch` case.
#[derive(Clone, Debug)]
pub struct Case {
    pub var: Option<QN>,
    pub ty: SeqType,
    pub ret: Expr,
}

/// A `catch` clause: error-code name tests (`None` local = wildcard) and the handler.
#[derive(Clone, Debug)]
pub struct Catch {
    pub codes: Vec<Option<String>>,
    pub body: Expr,
}

/// XQuery expressions.
#[derive(Clone, Debug)]
pub enum Expr {
    Seq(Vec<Expr>),
    Lit(Atomic),
    Var(QN),
    ContextItem,
    Flwor { clauses: Vec<Clause>, ret: Box<Expr> },
    Quantified { every: bool, binds: Vec<(QN, Expr)>, sat: Box<Expr> },
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    Typeswitch { operand: Box<Expr>, cases: Vec<Case>, default_var: Option<QN>, default: Box<Expr> },
    TryCatch { body: Box<Expr>, catches: Vec<Catch> },
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    GenCmp(Cmp, Box<Expr>, Box<Expr>),
    ValCmp(Cmp, Box<Expr>, Box<Expr>),
    NodeCmp(NodeCmp, Box<Expr>, Box<Expr>),
    Concat(Box<Expr>, Box<Expr>),
    Range(Box<Expr>, Box<Expr>),
    Arith(Arith, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Plus(Box<Expr>),
    Set(SetOp, Box<Expr>, Box<Expr>),
    InstanceOf(Box<Expr>, SeqType),
    Treat(Box<Expr>, SeqType),
    Castable(Box<Expr>, AtomicType, bool),
    Cast(Box<Expr>, AtomicType, bool),
    Path { start: PathStart, steps: Vec<(bool, Step)> },
    Filter(Box<Expr>, Vec<Expr>),
    SimpleMap(Box<Expr>, Box<Expr>),
    Call { name: QN, args: Vec<Expr> },
    /// Element constructor (direct or computed). `direct` controls atomic spacing:
    /// each content expression is handled separately.
    Elem { name: CtorName, ns: Vec<(String, String)>, attrs: Vec<(QN, String, Vec<AttrPart>)>, content: Vec<Expr> },
    Attr { name: CtorName, value: Box<Expr> },
    /// Literal text inside a direct constructor (not subject to atomic spacing).
    TextLit(String),
    Text(Box<Expr>),
    Comment(Box<Expr>),
    Pi { target: CtorName, value: Box<Expr> },
    Document(Box<Expr>),
}

/// A user-defined function.
#[derive(Clone, Debug)]
pub struct Function {
    pub name: QN,
    pub params: Vec<(QN, Option<SeqType>)>,
    pub ret: Option<SeqType>,
    pub body: Expr,
}

/// A declared global variable.
#[derive(Clone, Debug)]
pub struct VarDecl {
    pub name: QN,
    pub ty: Option<SeqType>,
    /// The initializer (`:= expr`), or the default value of an external variable.
    pub value: Option<Expr>,
    pub external: bool,
}

/// A parsed main module.
#[derive(Clone, Debug, Default)]
pub struct Module {
    pub functions: HashMap<(QN, usize), Function>,
    pub vars: Vec<VarDecl>,
    pub body: Option<Expr>,
    /// `declare default order empty greatest`.
    pub empty_greatest: bool,
    /// Statically known namespaces after the prolog (for messages / result conversion).
    pub namespaces: Vec<(String, String)>,
}
