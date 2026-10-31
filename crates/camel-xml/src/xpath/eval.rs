//! XPath 1.0 evaluation.

use super::ast::*;
use super::{Ctx, Docs, XNode, XPathError, XValue};
use crate::dom::{Document, NodeType};

type R<T> = Result<T, XPathError>;

/// Format a number per XPath `string()` rules.
pub fn format_number(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        if n > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        }
    } else if n == 0.0 {
        "0".into()
    } else if n.fract() == 0.0 && n.abs() < 1e18 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

/// Convert a string per XPath `number()` rules (NaN when not a valid number).
pub fn string_to_number(s: &str) -> f64 {
    let t = s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r'));
    let body = t.strip_prefix('-').unwrap_or(t);
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit() || c == '.') || body.matches('.').count() > 1 || body == "." {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

fn rc_doc(docs: &Docs, d: u32) -> std::rc::Rc<Document> {
    docs.get(d)
}

pub(crate) fn test_matches(docs: &Docs, n: XNode, test: &NodeTest, axis: Axis) -> bool {
    if n.ns != 0 {
        // namespace node: principal type only on the namespace axis
        return match test {
            NodeTest::Node => true,
            NodeTest::AnyName => axis == Axis::Namespace,
            NodeTest::Name(uri, local) => axis == Axis::Namespace && uri.is_empty() && docs.node_name(n).1 == *local,
            _ => false,
        };
    }
    let doc = rc_doc(docs, n.doc);
    let node = doc.node(n.node_id());
    let principal = if axis == Axis::Attribute { NodeType::Attribute } else { NodeType::Element };
    match test {
        NodeTest::Node => true,
        NodeTest::Text => node.ty == NodeType::Text,
        NodeTest::Comment => node.ty == NodeType::Comment,
        NodeTest::Pi(t) => node.ty == NodeType::ProcessingInstruction && t.as_ref().map(|t| *t == node.name.local).unwrap_or(true),
        NodeTest::AnyName => node.ty == principal && axis != Axis::Namespace,
        NodeTest::NsWildcard(uri) => node.ty == principal && axis != Axis::Namespace && node.name.uri == *uri,
        NodeTest::Name(uri, local) => node.ty == principal && axis != Axis::Namespace && node.name.local == *local && node.name.uri == *uri,
    }
}

fn push_descendants(doc: &Document, d: u32, id: usize, out: &mut Vec<XNode>) {
    let mut stack: Vec<usize> = doc.children(id).iter().rev().copied().collect();
    while let Some(c) = stack.pop() {
        out.push(XNode::new(d, c));
        stack.extend(doc.children(c).iter().rev().copied());
    }
}

fn parent_of(doc: &Document, n: XNode) -> Option<XNode> {
    if n.ns != 0 {
        return Some(XNode::new(n.doc, n.node_id()));
    }
    doc.parent(n.node_id()).map(|p| XNode::new(n.doc, p))
}

fn is_attr_or_ns(doc: &Document, n: XNode) -> bool {
    n.ns != 0 || doc.node_type(n.node_id()) == NodeType::Attribute
}

pub(crate) fn axis_nodes(docs: &Docs, n: XNode, axis: Axis) -> Vec<XNode> {
    let doc = rc_doc(docs, n.doc);
    let d = n.doc;
    let mut out = Vec::new();
    match axis {
        Axis::SelfAxis => out.push(n),
        Axis::Child => {
            if n.ns == 0 {
                out.extend(doc.children(n.node_id()).iter().map(|&c| XNode::new(d, c)));
            }
        }
        Axis::Descendant => {
            if n.ns == 0 {
                push_descendants(&doc, d, n.node_id(), &mut out);
            }
        }
        Axis::DescendantOrSelf => {
            out.push(n);
            if n.ns == 0 {
                push_descendants(&doc, d, n.node_id(), &mut out);
            }
        }
        Axis::Parent => out.extend(parent_of(&doc, n)),
        Axis::Ancestor | Axis::AncestorOrSelf => {
            if axis == Axis::AncestorOrSelf {
                out.push(n);
            }
            let mut cur = parent_of(&doc, n);
            while let Some(p) = cur {
                out.push(p);
                cur = parent_of(&doc, p);
            }
        }
        Axis::Attribute => {
            if n.ns == 0 && doc.node_type(n.node_id()) == NodeType::Element {
                out.extend(doc.attributes(n.node_id()).iter().map(|&a| XNode::new(d, a)));
            }
        }
        Axis::Namespace => {
            if n.ns == 0 && doc.node_type(n.node_id()) == NodeType::Element {
                let k = doc.in_scope_namespaces(n.node_id()).len();
                out.extend((1..=k).map(|i| XNode { doc: d, id: n.id, ns: i as u32 }));
            }
        }
        Axis::FollowingSibling | Axis::PrecedingSibling => {
            if is_attr_or_ns(&doc, n) {
                return out;
            }
            if let Some(p) = doc.parent(n.node_id()) {
                let sibs = doc.children(p);
                if let Some(i) = sibs.iter().position(|&c| c == n.node_id()) {
                    if axis == Axis::FollowingSibling {
                        out.extend(sibs[i + 1..].iter().map(|&c| XNode::new(d, c)));
                    } else {
                        out.extend(sibs[..i].iter().rev().map(|&c| XNode::new(d, c)));
                    }
                }
            }
        }
        Axis::Following => {
            let mut cur = n;
            if is_attr_or_ns(&doc, n) {
                let el = parent_of(&doc, n).unwrap();
                push_descendants(&doc, d, el.node_id(), &mut out);
                cur = el;
            }
            let mut c = cur.node_id();
            while let Some(p) = doc.parent(c) {
                let sibs = doc.children(p);
                if let Some(i) = sibs.iter().position(|&x| x == c) {
                    for &s in &sibs[i + 1..] {
                        out.push(XNode::new(d, s));
                        push_descendants(&doc, d, s, &mut out);
                    }
                }
                c = p;
            }
        }
        Axis::Preceding => {
            let target = if is_attr_or_ns(&doc, n) { parent_of(&doc, n).unwrap() } else { n };
            let mut ancestors = Vec::new();
            let mut cur = doc.parent(target.node_id());
            while let Some(p) = cur {
                ancestors.push(p);
                cur = doc.parent(p);
            }
            let root = doc.tree_root(target.node_id());
            // pre-order walk until the target
            let mut stack = vec![root];
            while let Some(c) = stack.pop() {
                if c == target.node_id() {
                    break;
                }
                if !ancestors.contains(&c) {
                    out.push(XNode::new(d, c));
                }
                stack.extend(doc.children(c).iter().rev().copied());
            }
            out.reverse();
        }
    }
    out
}

fn str_of(ctx: &Ctx, v: &XValue) -> String {
    v.to_string_value(ctx.docs)
}

fn num_of(ctx: &Ctx, v: &XValue) -> f64 {
    v.to_number(ctx.docs)
}

fn nodes(v: XValue, what: &str) -> R<Vec<XNode>> {
    match v {
        XValue::Nodes(n) => Ok(n),
        other => Err(XPathError::eval(format!("{} requires a node-set but got a {}", what, other.type_name()))),
    }
}

/// Apply predicates to a node list given in axis order.
fn apply_predicates(ctx: &Ctx, mut list: Vec<XNode>, preds: &[Expr]) -> R<Vec<XNode>> {
    for p in preds {
        let size = list.len();
        let mut keep = Vec::with_capacity(size);
        for (i, &n) in list.iter().enumerate() {
            let c = Ctx { docs: ctx.docs, node: n, pos: i + 1, size, env: ctx.env };
            let v = eval(p, &c)?;
            let ok = match v {
                XValue::Num(x) => x == (i + 1) as f64,
                other => other.to_bool(),
            };
            if ok {
                keep.push(n);
            }
        }
        list = keep;
    }
    Ok(list)
}

fn eval_steps(ctx: &Ctx, start: Vec<XNode>, steps: &[Step]) -> R<Vec<XNode>> {
    let mut cur = start;
    let mut i = 0;
    while i < steps.len() {
        let mut step = &steps[i];
        let mut axis = step.axis;
        // `//x` without positional predicates: descendant::x
        if step.axis == Axis::DescendantOrSelf && step.test == NodeTest::Node && step.preds.is_empty() && i + 1 < steps.len() {
            let next = &steps[i + 1];
            if next.axis == Axis::Child && next.preds.iter().all(|p| !p.is_positional()) {
                axis = Axis::Descendant;
                step = next;
                i += 1;
            }
        }
        let mut next = Vec::new();
        for &n in &cur {
            let mut list: Vec<XNode> = axis_nodes(ctx.docs, n, axis).into_iter().filter(|&m| test_matches(ctx.docs, m, &step.test, axis)).collect();
            if !step.preds.is_empty() {
                list = apply_predicates(ctx, list, &step.preds)?;
            }
            next.extend(list);
        }
        next.sort();
        next.dedup();
        cur = next;
        i += 1;
    }
    Ok(cur)
}

fn compare_atomic(op: CmpOp, a: &XValue, b: &XValue, ctx: &Ctx) -> bool {
    match op {
        CmpOp::Eq | CmpOp::Ne => {
            let eq = if matches!(a, XValue::Bool(_)) || matches!(b, XValue::Bool(_)) {
                a.to_bool() == b.to_bool()
            } else if matches!(a, XValue::Num(_)) || matches!(b, XValue::Num(_)) {
                num_of(ctx, a) == num_of(ctx, b)
            } else {
                str_of(ctx, a) == str_of(ctx, b)
            };
            if op == CmpOp::Eq {
                eq
            } else {
                !eq
            }
        }
        _ => {
            let (x, y) = (num_of(ctx, a), num_of(ctx, b));
            match op {
                CmpOp::Lt => x < y,
                CmpOp::Le => x <= y,
                CmpOp::Gt => x > y,
                _ => x >= y,
            }
        }
    }
}

fn compare(op: CmpOp, a: &XValue, b: &XValue, ctx: &Ctx) -> bool {
    match (a, b) {
        (XValue::Nodes(x), XValue::Nodes(y)) => {
            let ys: Vec<String> = y.iter().map(|n| ctx.docs.string_value(*n)).collect();
            x.iter().any(|n| {
                let sx = XValue::Str(ctx.docs.string_value(*n));
                ys.iter().any(|sy| compare_atomic(op, &sx, &XValue::Str(sy.clone()), ctx))
            })
        }
        (XValue::Nodes(x), other) => match other {
            XValue::Bool(_) => compare_atomic(op, &XValue::Bool(!x.is_empty()), other, ctx),
            XValue::Num(_) => x.iter().any(|n| compare_atomic(op, &XValue::Num(super::string_to_number(&ctx.docs.string_value(*n))), other, ctx)),
            _ => x.iter().any(|n| compare_atomic(op, &XValue::Str(ctx.docs.string_value(*n)), other, ctx)),
        },
        (other, XValue::Nodes(_)) => {
            let flipped = match op {
                CmpOp::Lt => CmpOp::Gt,
                CmpOp::Le => CmpOp::Ge,
                CmpOp::Gt => CmpOp::Lt,
                CmpOp::Ge => CmpOp::Le,
                o => o,
            };
            compare(flipped, b, other, ctx)
        }
        _ => compare_atomic(op, a, b, ctx),
    }
}

pub(crate) fn eval(e: &Expr, ctx: &Ctx) -> R<XValue> {
    Ok(match e {
        Expr::Or(a, b) => XValue::Bool(eval(a, ctx)?.to_bool() || eval(b, ctx)?.to_bool()),
        Expr::And(a, b) => XValue::Bool(eval(a, ctx)?.to_bool() && eval(b, ctx)?.to_bool()),
        Expr::Cmp(op, a, b) => {
            let (x, y) = (eval(a, ctx)?, eval(b, ctx)?);
            XValue::Bool(compare(*op, &x, &y, ctx))
        }
        Expr::Arith(op, a, b) => {
            let x = num_of(ctx, &eval(a, ctx)?);
            let y = num_of(ctx, &eval(b, ctx)?);
            XValue::Num(match op {
                ArithOp::Add => x + y,
                ArithOp::Sub => x - y,
                ArithOp::Mul => x * y,
                ArithOp::Div => x / y,
                ArithOp::Mod => x % y,
            })
        }
        Expr::Neg(a) => XValue::Num(-num_of(ctx, &eval(a, ctx)?)),
        Expr::Union(a, b) => {
            let mut x = nodes(eval(a, ctx)?, "The union operator")?;
            x.extend(nodes(eval(b, ctx)?, "The union operator")?);
            x.sort();
            x.dedup();
            XValue::Nodes(x)
        }
        Expr::Literal(s) => XValue::Str(s.clone()),
        Expr::Number(n) => XValue::Num(*n),
        Expr::Var(uri, local) => ctx.env.variable(uri, local)?,
        Expr::Func { uri, local, prefix, args } => call(uri, local, prefix.as_deref(), args, ctx)?,
        Expr::Filter(prim, preds) => {
            let mut ns = nodes(eval(prim, ctx)?, "A predicate")?;
            ns.sort();
            ns.dedup();
            XValue::Nodes(apply_predicates(ctx, ns, preds)?)
        }
        Expr::Path(p) => {
            let start = match &p.start {
                PathStart::Root => {
                    let doc = rc_doc(ctx.docs, ctx.node.doc);
                    vec![XNode::new(ctx.node.doc, doc.tree_root(ctx.node.node_id()))]
                }
                PathStart::Context => vec![ctx.node],
                PathStart::Expr(e) => {
                    let mut v = nodes(eval(e, ctx)?, "A path step")?;
                    v.sort();
                    v.dedup();
                    v
                }
            };
            XValue::Nodes(eval_steps(ctx, start, &p.steps)?)
        }
    })
}

fn arg_nodes_or_ctx(args: &[Expr], ctx: &Ctx, f: &str) -> R<Option<XNode>> {
    if args.is_empty() {
        return Ok(Some(ctx.node));
    }
    let n = nodes(eval(&args[0], ctx)?, f)?;
    Ok(n.into_iter().min())
}

fn arg_str(args: &[Expr], i: usize, ctx: &Ctx) -> R<String> {
    Ok(str_of(ctx, &eval(&args[i], ctx)?))
}

fn round_half_up(x: f64) -> f64 {
    if x.is_nan() || x.is_infinite() {
        return x;
    }
    if (-0.5..0.0).contains(&x) {
        return -0.0;
    }
    (x + 0.5).floor()
}

fn call(uri: &str, local: &str, prefix: Option<&str>, args: &[Expr], ctx: &Ctx) -> R<XValue> {
    // extension / environment functions first (they receive evaluated arguments)
    if !uri.is_empty() || ctx_env_overrides(local) {
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            vals.push(eval(a, ctx)?);
        }
        if let Some(r) = ctx.env.function(uri, local, &vals, ctx) {
            return r;
        }
        if !uri.is_empty() {
            let q = match prefix {
                Some(p) => format!("{}:{}", p, local),
                None => local.to_string(),
            };
            return Err(XPathError::eval(format!("Could not find function: {}", q)));
        }
    }
    let n = args.len();
    Ok(match local {
        "last" => XValue::Num(ctx.size as f64),
        "position" => XValue::Num(ctx.pos as f64),
        "count" => XValue::Num(nodes(eval(&args[0], ctx)?, "count()")?.len() as f64),
        "id" => {
            let v = eval(&args[0], ctx)?;
            let mut tokens: Vec<String> = Vec::new();
            match &v {
                XValue::Nodes(ns) => {
                    for x in ns {
                        tokens.extend(ctx.docs.string_value(*x).split_whitespace().map(String::from));
                    }
                }
                other => tokens.extend(str_of(ctx, other).split_whitespace().map(String::from)),
            }
            let doc = rc_doc(ctx.docs, ctx.node.doc);
            let mut out: Vec<XNode> = tokens.iter().filter_map(|t| doc.ids.get(t)).map(|&id| XNode::new(ctx.node.doc, id)).collect();
            out.sort();
            out.dedup();
            XValue::Nodes(out)
        }
        "local-name" => XValue::Str(arg_nodes_or_ctx(args, ctx, "local-name()")?.map(|x| ctx.docs.node_name(x).1).unwrap_or_default()),
        "namespace-uri" => XValue::Str(arg_nodes_or_ctx(args, ctx, "namespace-uri()")?.map(|x| ctx.docs.node_name(x).2).unwrap_or_default()),
        "name" => XValue::Str(
            arg_nodes_or_ctx(args, ctx, "name()")?
                .map(|x| {
                    let (p, l, _) = ctx.docs.node_name(x);
                    if p.is_empty() {
                        l
                    } else {
                        format!("{}:{}", p, l)
                    }
                })
                .unwrap_or_default(),
        ),
        "string" => {
            if n == 0 {
                XValue::Str(ctx.docs.string_value(ctx.node))
            } else {
                XValue::Str(arg_str(args, 0, ctx)?)
            }
        }
        "concat" => {
            let mut s = String::new();
            for i in 0..n {
                s.push_str(&arg_str(args, i, ctx)?);
            }
            XValue::Str(s)
        }
        "starts-with" => XValue::Bool(arg_str(args, 0, ctx)?.starts_with(&arg_str(args, 1, ctx)?)),
        "ends-with" => XValue::Bool(arg_str(args, 0, ctx)?.ends_with(&arg_str(args, 1, ctx)?)),
        "contains" => XValue::Bool(arg_str(args, 0, ctx)?.contains(&arg_str(args, 1, ctx)?)),
        "substring-before" => {
            let (s, t) = (arg_str(args, 0, ctx)?, arg_str(args, 1, ctx)?);
            XValue::Str(s.find(&t).map(|i| s[..i].to_string()).unwrap_or_default())
        }
        "substring-after" => {
            let (s, t) = (arg_str(args, 0, ctx)?, arg_str(args, 1, ctx)?);
            XValue::Str(s.find(&t).map(|i| s[i + t.len()..].to_string()).unwrap_or_default())
        }
        "substring" => {
            let s: Vec<char> = arg_str(args, 0, ctx)?.chars().collect();
            let start = round_half_up(num_of(ctx, &eval(&args[1], ctx)?));
            let end = if n == 3 { start + round_half_up(num_of(ctx, &eval(&args[2], ctx)?)) } else { f64::INFINITY };
            let mut out = String::new();
            for (i, c) in s.iter().enumerate() {
                let p = (i + 1) as f64;
                if p >= start && p < end {
                    out.push(*c);
                }
            }
            XValue::Str(out)
        }
        "string-length" => {
            let s = if n == 0 { ctx.docs.string_value(ctx.node) } else { arg_str(args, 0, ctx)? };
            XValue::Num(s.chars().count() as f64)
        }
        "normalize-space" => {
            let s = if n == 0 { ctx.docs.string_value(ctx.node) } else { arg_str(args, 0, ctx)? };
            XValue::Str(s.split(|c| matches!(c, ' ' | '\t' | '\n' | '\r')).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" "))
        }
        "translate" => {
            let s = arg_str(args, 0, ctx)?;
            let from: Vec<char> = arg_str(args, 1, ctx)?.chars().collect();
            let to: Vec<char> = arg_str(args, 2, ctx)?.chars().collect();
            let mut out = String::new();
            for c in s.chars() {
                match from.iter().position(|&f| f == c) {
                    Some(i) => {
                        if let Some(&t) = to.get(i) {
                            out.push(t);
                        }
                    }
                    None => out.push(c),
                }
            }
            XValue::Str(out)
        }
        "upper-case" => XValue::Str(arg_str(args, 0, ctx)?.to_uppercase()),
        "lower-case" => XValue::Str(arg_str(args, 0, ctx)?.to_lowercase()),
        "boolean" => XValue::Bool(eval(&args[0], ctx)?.to_bool()),
        "not" => XValue::Bool(!eval(&args[0], ctx)?.to_bool()),
        "true" => XValue::Bool(true),
        "false" => XValue::Bool(false),
        "lang" => {
            let want = arg_str(args, 0, ctx)?.to_ascii_lowercase();
            let doc = rc_doc(ctx.docs, ctx.node.doc);
            let lang = doc.lang(ctx.node.node_id()).map(|l| l.to_ascii_lowercase());
            XValue::Bool(match lang {
                Some(l) => l == want || (l.starts_with(&want) && l[want.len()..].starts_with('-')),
                None => false,
            })
        }
        "number" => {
            if n == 0 {
                XValue::Num(super::string_to_number(&ctx.docs.string_value(ctx.node)))
            } else {
                XValue::Num(num_of(ctx, &eval(&args[0], ctx)?))
            }
        }
        "sum" => {
            let ns = nodes(eval(&args[0], ctx)?, "sum()")?;
            XValue::Num(ns.iter().map(|x| super::string_to_number(&ctx.docs.string_value(*x))).sum())
        }
        "floor" => XValue::Num(num_of(ctx, &eval(&args[0], ctx)?).floor()),
        "ceiling" => XValue::Num(num_of(ctx, &eval(&args[0], ctx)?).ceil()),
        "round" => XValue::Num(round_half_up(num_of(ctx, &eval(&args[0], ctx)?))),
        _ => {
            // unknown core-namespace function: give the environment a chance
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                vals.push(eval(a, ctx)?);
            }
            if let Some(r) = ctx.env.function("", local, &vals, ctx) {
                return r;
            }
            return Err(XPathError::eval(format!("Could not find function: {}", local)));
        }
    })
}

/// Core-namespace names an environment may implement (XSLT functions).
fn ctx_env_overrides(local: &str) -> bool {
    matches!(
        local,
        "key" | "document" | "current" | "generate-id" | "format-number" | "system-property" | "element-available" | "function-available" | "unparsed-entity-uri"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(format_number(1.0), "1");
        assert_eq!(format_number(-0.0), "0");
        assert_eq!(format_number(0.5), "0.5");
        assert_eq!(format_number(f64::NAN), "NaN");
        assert_eq!(format_number(-f64::INFINITY), "-Infinity");
        assert!(string_to_number("1e3").is_nan());
        assert!(string_to_number("+1").is_nan());
        assert_eq!(string_to_number(" -2.5 "), -2.5);
        assert_eq!(string_to_number(".5"), 0.5);
        assert_eq!(round_half_up(2.5), 3.0);
        assert_eq!(round_half_up(-2.5), -2.0);
    }
}
