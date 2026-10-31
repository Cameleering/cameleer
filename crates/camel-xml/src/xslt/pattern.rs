//! XSLT 1.0 match patterns (`match`, `count`, `from`, key `match`).

use crate::xpath::ast::{Axis, Expr, NodeTest, PathStart, Step};
use crate::xpath::{axis_nodes, eval_expr, node_test_matches, Ctx, Docs, Env, XNode, XPath, XPathError, XValue};

/// How a step is connected to the previous one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sep {
    /// First step of a relative pattern.
    None,
    /// `/`
    Child,
    /// `//`
    Desc,
}

/// Anchor of a location path pattern.
#[derive(Clone, Debug)]
pub enum Anchor {
    /// Relative pattern (`a/b`).
    Relative,
    /// Starts at the root (`/a`, `//a`), or is just `/`.
    Root,
    /// Starts with `id(...)` or `key(...)`.
    Func(Expr),
}

/// One location path pattern (an alternative of a union pattern).
#[derive(Clone, Debug)]
pub struct PathPattern {
    pub anchor: Anchor,
    /// Steps left to right with the separator before each.
    pub steps: Vec<(Sep, Step)>,
}

/// A union pattern.
#[derive(Clone, Debug)]
pub struct Pattern {
    pub text: String,
    pub alts: Vec<PathPattern>,
}

fn perr(text: &str, m: &str) -> String {
    format!("Invalid pattern '{}': {}", text, m)
}

fn convert_steps(text: &str, steps: &[Step], first_sep: Sep) -> Result<Vec<(Sep, Step)>, String> {
    let mut out = Vec::new();
    let mut sep = first_sep;
    for s in steps {
        if s.axis == Axis::DescendantOrSelf && s.test == NodeTest::Node && s.preds.is_empty() {
            sep = Sep::Desc;
            continue;
        }
        if s.axis != Axis::Child && s.axis != Axis::Attribute {
            return Err(perr(text, "only the child and attribute axes are allowed in patterns"));
        }
        out.push((sep, s.clone()));
        sep = Sep::Child;
    }
    if sep == Sep::Desc {
        return Err(perr(text, "a pattern must not end with '//'"));
    }
    Ok(out)
}

fn is_id_key(e: &Expr) -> bool {
    matches!(e, Expr::Func { uri, local, args, .. } if uri.is_empty() && (local == "id" || local == "key") && args.iter().all(|a| matches!(a, Expr::Literal(_) | Expr::Var(..))))
}

fn alt_from_expr(text: &str, e: &Expr, out: &mut Vec<PathPattern>) -> Result<(), String> {
    match e {
        Expr::Union(a, b) => {
            alt_from_expr(text, a, out)?;
            alt_from_expr(text, b, out)
        }
        Expr::Path(p) => {
            let (anchor, first) = match &p.start {
                PathStart::Root => (Anchor::Root, Sep::Child),
                PathStart::Context => (Anchor::Relative, Sep::None),
                PathStart::Expr(f) if is_id_key(f) => (Anchor::Func((**f).clone()), Sep::Child),
                PathStart::Expr(_) => return Err(perr(text, "only id() and key() may start a pattern")),
            };
            let steps = convert_steps(text, &p.steps, first)?;
            if matches!(anchor, Anchor::Relative) && steps.first().map(|(s, _)| *s != Sep::None).unwrap_or(true) {
                // relative path starting with '//' is impossible; keep as is
            }
            out.push(PathPattern { anchor, steps });
            Ok(())
        }
        f if is_id_key(f) => {
            out.push(PathPattern { anchor: Anchor::Func(f.clone()), steps: Vec::new() });
            Ok(())
        }
        _ => Err(perr(text, "not a location path pattern")),
    }
}

impl Pattern {
    /// Parse a pattern with a namespace prefix resolver.
    pub fn parse(text: &str, ns: crate::xpath::PrefixResolver<'_>) -> Result<Pattern, String> {
        let xp = XPath::compile_with(text, ns).map_err(|e| e.message)?;
        let mut alts = Vec::new();
        alt_from_expr(text, xp.expr(), &mut alts)?;
        Ok(Pattern { text: text.to_string(), alts })
    }

    /// Default priority of one alternative.
    pub fn default_priority(alt: &PathPattern) -> f64 {
        if !matches!(alt.anchor, Anchor::Relative) || alt.steps.len() != 1 {
            return 0.5;
        }
        let (_, step) = &alt.steps[0];
        if !step.preds.is_empty() {
            return 0.5;
        }
        match &step.test {
            NodeTest::Name(..) => 0.0,
            NodeTest::Pi(Some(_)) => 0.0,
            NodeTest::NsWildcard(_) => -0.25,
            _ => -0.5,
        }
    }

    /// Whether any alternative matches the node.
    pub fn matches(&self, docs: &Docs, n: XNode, env: &dyn Env) -> Result<bool, XPathError> {
        for alt in &self.alts {
            if alt_matches(docs, alt, n, env)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl PathPattern {
    /// Whether this alternative matches the node.
    pub fn matches(&self, docs: &Docs, n: XNode, env: &dyn Env) -> Result<bool, XPathError> {
        alt_matches(docs, self, n, env)
    }
}

fn parent(docs: &Docs, n: XNode) -> Option<XNode> {
    axis_nodes(docs, n, Axis::Parent).into_iter().next()
}

fn step_matches(docs: &Docs, step: &Step, n: XNode, env: &dyn Env) -> Result<bool, XPathError> {
    let is_attr = docs.node_type(n) == Some(crate::dom::NodeType::Attribute);
    if n.is_namespace() {
        return Ok(false);
    }
    match step.axis {
        Axis::Attribute if !is_attr => return Ok(false),
        Axis::Child if is_attr || docs.node_type(n) == Some(crate::dom::NodeType::Document) => return Ok(false),
        _ => {}
    }
    if !node_test_matches(docs, n, &step.test, step.axis) {
        return Ok(false);
    }
    if step.preds.is_empty() {
        return Ok(true);
    }
    let Some(p) = parent(docs, n) else { return Ok(false) };
    let mut list: Vec<XNode> = axis_nodes(docs, p, step.axis).into_iter().filter(|&m| node_test_matches(docs, m, &step.test, step.axis)).collect();
    for pred in &step.preds {
        let size = list.len();
        let mut keep = Vec::new();
        for (i, &m) in list.iter().enumerate() {
            let ctx = Ctx { docs, node: m, pos: i + 1, size, env };
            let ok = match eval_expr(pred, &ctx)? {
                XValue::Num(x) => x == (i + 1) as f64,
                v => v.to_bool(),
            };
            if ok {
                keep.push(m);
            }
        }
        list = keep;
    }
    Ok(list.contains(&n))
}

fn func_nodes(docs: &Docs, e: &Expr, n: XNode, env: &dyn Env) -> Result<Vec<XNode>, XPathError> {
    let ctx = Ctx { docs, node: n, pos: 1, size: 1, env };
    match eval_expr(e, &ctx)? {
        XValue::Nodes(v) => Ok(v),
        _ => Ok(Vec::new()),
    }
}

fn alt_matches(docs: &Docs, alt: &PathPattern, n: XNode, env: &dyn Env) -> Result<bool, XPathError> {
    if alt.steps.is_empty() {
        return match &alt.anchor {
            Anchor::Root => Ok(docs.node_type(n) == Some(crate::dom::NodeType::Document)),
            Anchor::Func(e) => Ok(func_nodes(docs, e, n, env)?.contains(&n)),
            Anchor::Relative => Ok(false),
        };
    }
    match_from(docs, alt, alt.steps.len() - 1, n, env)
}

fn match_from(docs: &Docs, alt: &PathPattern, i: usize, n: XNode, env: &dyn Env) -> Result<bool, XPathError> {
    let (sep, step) = &alt.steps[i];
    if !step_matches(docs, step, n, env)? {
        return Ok(false);
    }
    if i == 0 {
        return match &alt.anchor {
            Anchor::Relative => Ok(true),
            Anchor::Root => match sep {
                Sep::Child => Ok(parent(docs, n).map(|p| docs.node_type(p) == Some(crate::dom::NodeType::Document)).unwrap_or(false)),
                _ => {
                    let mut cur = parent(docs, n);
                    while let Some(p) = cur {
                        if docs.node_type(p) == Some(crate::dom::NodeType::Document) {
                            return Ok(true);
                        }
                        cur = parent(docs, p);
                    }
                    Ok(false)
                }
            },
            Anchor::Func(e) => {
                let set = func_nodes(docs, e, n, env)?;
                match sep {
                    Sep::Child => Ok(parent(docs, n).map(|p| set.contains(&p)).unwrap_or(false)),
                    _ => {
                        let mut cur = parent(docs, n);
                        while let Some(p) = cur {
                            if set.contains(&p) {
                                return Ok(true);
                            }
                            cur = parent(docs, p);
                        }
                        Ok(false)
                    }
                }
            }
        };
    }
    match sep {
        Sep::Child | Sep::None => match parent(docs, n) {
            Some(p) => match_from(docs, alt, i - 1, p, env),
            None => Ok(false),
        },
        Sep::Desc => {
            let mut cur = parent(docs, n);
            while let Some(p) = cur {
                if match_from(docs, alt, i - 1, p, env)? {
                    return Ok(true);
                }
                cur = parent(docs, p);
            }
            Ok(false)
        }
    }
}
