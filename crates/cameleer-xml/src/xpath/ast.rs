//! XPath 1.0 abstract syntax tree. Names are resolved to `(namespace URI, local name)` at
//! compile time.

/// Comparison operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Arithmetic operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

/// The 13 XPath axes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Ancestor,
    AncestorOrSelf,
    Attribute,
    Child,
    Descendant,
    DescendantOrSelf,
    Following,
    FollowingSibling,
    Namespace,
    Parent,
    Preceding,
    PrecedingSibling,
    SelfAxis,
}

impl Axis {
    pub fn from_name(n: &str) -> Option<Axis> {
        Some(match n {
            "ancestor" => Axis::Ancestor,
            "ancestor-or-self" => Axis::AncestorOrSelf,
            "attribute" => Axis::Attribute,
            "child" => Axis::Child,
            "descendant" => Axis::Descendant,
            "descendant-or-self" => Axis::DescendantOrSelf,
            "following" => Axis::Following,
            "following-sibling" => Axis::FollowingSibling,
            "namespace" => Axis::Namespace,
            "parent" => Axis::Parent,
            "preceding" => Axis::Preceding,
            "preceding-sibling" => Axis::PrecedingSibling,
            "self" => Axis::SelfAxis,
            _ => return None,
        })
    }

    /// Reverse axes deliver nodes in reverse document order.
    pub fn is_reverse(self) -> bool {
        matches!(self, Axis::Ancestor | Axis::AncestorOrSelf | Axis::Preceding | Axis::PrecedingSibling)
    }
}

/// Node tests.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeTest {
    /// `*`
    AnyName,
    /// `prefix:*` (resolved URI)
    NsWildcard(String),
    /// QName (resolved URI, local name)
    Name(String, String),
    Node,
    Text,
    Comment,
    Pi(Option<String>),
}

/// A location step.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub axis: Axis,
    pub test: NodeTest,
    pub preds: Vec<Expr>,
}

impl Step {
    pub fn descendant_or_self() -> Step {
        Step { axis: Axis::DescendantOrSelf, test: NodeTest::Node, preds: Vec::new() }
    }
}

/// Where a path starts.
#[derive(Clone, Debug, PartialEq)]
pub enum PathStart {
    /// `/...`: the root of the context node's tree.
    Root,
    /// A relative path: the context node.
    Context,
    /// A filter expression followed by `/` or `//`.
    Expr(Box<Expr>),
}

/// A path expression.
#[derive(Clone, Debug, PartialEq)]
pub struct PathExpr {
    pub start: PathStart,
    pub steps: Vec<Step>,
}

/// XPath expressions.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Cmp(CmpOp, Box<Expr>, Box<Expr>),
    Arith(ArithOp, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Union(Box<Expr>, Box<Expr>),
    Literal(String),
    Number(f64),
    Var(String, String),
    Func { uri: String, local: String, prefix: Option<String>, args: Vec<Expr> },
    Filter(Box<Expr>, Vec<Expr>),
    Path(PathExpr),
    // ---- XPath 2.0 (compiled with `XPath::compile_v2`) ----
    /// `if (c) then a else b`
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `for $v in E, $w in F return R` (bindings `(uri, local, expr)`)
    For(Vec<(String, String, Expr)>, Box<Expr>),
    /// `some` (false) / `every` (true) `$v in E satisfies T`
    Quantified(bool, Vec<(String, String, Expr)>, Box<Expr>),
    /// `(a, b, ...)`, `()`
    Sequence(Vec<Expr>),
    /// `a to b`
    Range(Box<Expr>, Box<Expr>),
    /// `a intersect b`
    Intersect(Box<Expr>, Box<Expr>),
    /// `a except b`
    Except(Box<Expr>, Box<Expr>),
}

impl Expr {
    /// Whether a predicate may depend on the context position or size: it may evaluate
    /// to a number, or uses `position()` / `last()` outside nested predicates.
    pub fn is_positional(&self) -> bool {
        match self {
            Expr::Number(_) | Expr::Arith(..) | Expr::Neg(_) | Expr::Var(..) => true,
            Expr::Func { uri, local, .. } if !uri.is_empty() => {
                let _ = local;
                true
            }
            Expr::Func { local, .. }
                if matches!(local.as_str(), "position" | "last" | "count" | "sum" | "number" | "string-length" | "floor" | "ceiling" | "round") =>
            {
                true
            }
            _ => self.uses_position(),
        }
    }

    fn uses_position(&self) -> bool {
        match self {
            Expr::Func { uri, local, args, .. } => (uri.is_empty() && (local == "position" || local == "last")) || args.iter().any(|a| a.uses_position()),
            Expr::Or(a, b) | Expr::And(a, b) | Expr::Cmp(_, a, b) | Expr::Arith(_, a, b) | Expr::Union(a, b) | Expr::Range(a, b) | Expr::Intersect(a, b) | Expr::Except(a, b) => a.uses_position() || b.uses_position(),
            Expr::Neg(a) => a.uses_position(),
            Expr::Filter(p, _) => p.uses_position(),
            Expr::Path(p) => matches!(&p.start, PathStart::Expr(e) if e.uses_position()),
            Expr::Literal(_) | Expr::Number(_) | Expr::Var(..) => false,
            Expr::If(c, a, b) => c.uses_position() || a.uses_position() || b.uses_position(),
            Expr::For(bs, r) | Expr::Quantified(_, bs, r) => bs.iter().any(|(_, _, e)| e.uses_position()) || r.uses_position(),
            Expr::Sequence(es) => es.iter().any(|e| e.uses_position()),

        }
    }
}
