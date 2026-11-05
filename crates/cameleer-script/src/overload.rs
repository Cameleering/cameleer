//! Method overloading by parameter type (Groovy, Java): methods / functions declared several
//! times with the same name form an overload set, and each call picks the most specific
//! candidate for the runtime types of its arguments (Groovy's multi-methods; for Java the
//! static type of the argument expressions is approximated by their runtime types).

use std::cell::RefCell;
use std::rc::Rc;

use crate::interp::Interp;
use crate::value::*;

/// The overload candidates of a dispatcher function.
pub fn candidates(c: &Closure) -> Option<Vec<V>> {
    match c.props.borrow().get("%overloads") {
        Some(V::Arr(a)) => Some(a.borrow().clone()),
        _ => None,
    }
}

fn class_names(v: &V) -> Vec<String> {
    let mut out = vec![];
    let mut cur = crate::classes::class_of(v);
    while let Some(c) = cur {
        if let Some(n) = &c.def.name {
            out.push(n.clone());
        }
        cur = match &c.parent {
            Some(V::Func(p)) => Some(p.clone()),
            _ => None,
        };
    }
    out
}

/// How well an argument matches a declared parameter type (higher is more specific; None
/// when it does not match).
pub fn type_score(it: &Interp, v: &V, ty: Option<&str>) -> Option<u32> {
    let Some(ty) = ty else { return Some(1) };
    let ty = ty.strip_prefix("java.lang.").unwrap_or(ty);
    if matches!(ty, "def" | "var" | "Object" | "T" | "E" | "K") {
        return Some(1);
    }
    let primitive = matches!(ty, "int" | "long" | "double" | "float" | "boolean" | "char" | "byte" | "short");
    if v.is_nullish() {
        return if primitive { None } else { Some(1) };
    }
    let s = match (ty, v) {
        ("String", V::Str(_)) => 4,
        ("CharSequence" | "GString" | "Comparable", V::Str(_)) => 3,
        ("char" | "Character", V::Str(s)) if s.chars().count() == 1 => 5,
        ("int" | "Integer", V::Int(i)) if *i >= i32::MIN as i64 && *i <= i32::MAX as i64 => 4,
        ("long" | "Long", V::Int(_)) => 3,
        ("short" | "Short" | "byte" | "Byte", V::Int(_)) => 2,
        ("BigInteger", V::Int(_)) => 2,
        ("double" | "Double", V::Num(_)) => 4,
        ("float" | "Float" | "BigDecimal", V::Num(_)) => 3,
        ("double" | "Double" | "float" | "Float" | "BigDecimal", V::Int(_)) => 1,
        ("Number" | "Comparable", x) if x.is_number() => 2,
        ("boolean" | "Boolean", V::Bool(_)) => 4,
        ("Closure" | "Function" | "Runnable" | "Supplier" | "Consumer" | "Predicate" | "BiFunction" | "Callable", x) if x.is_callable() => 3,
        (t, x) if crate::builtins::is_type(x, t) => {
            if matches!(t, "List" | "Map" | "Set") {
                3
            } else {
                2
            }
        }
        (t, x) => {
            let names = class_names(x);
            if let Some(i) = names.iter().position(|n| n == t) {
                // the closer the class, the more specific
                10u32.saturating_sub(i as u32).max(2)
            } else if implements(it, x, t) {
                2
            } else {
                return None;
            }
        }
    };
    let _ = it;
    Some(s)
}

/// Whether an instance's class (or a parent) mixes in the trait / interface `t`.
fn implements(it: &Interp, v: &V, t: &str) -> bool {
    let mut cur = crate::classes::class_of(v);
    while let Some(c) = cur {
        if let Some(info) = it.classes.get(&(Rc::as_ptr(&c) as usize)) {
            if info.def.mixins.iter().any(|m| matches!(m, crate::ast::Expr::Ident(n, _) if n == t) || matches!(m, crate::ast::Expr::Type(n) if n == t)) {
                return true;
            }
        }
        cur = match &c.parent {
            Some(V::Func(p)) => Some(p.clone()),
            _ => None,
        };
    }
    false
}

/// Score of a whole candidate for the arguments (None when it is not applicable).
fn candidate_score(it: &Interp, def: &crate::ast::FuncDef, args: &[V]) -> Option<(u32, i32)> {
    let n = def.params.len();
    let required = def.params.iter().take_while(|p| p.default.is_none()).count();
    if args.len() < required || (args.len() > n && def.rest.is_none()) {
        return None;
    }
    let mut total = 0;
    for (p, a) in def.params.iter().zip(args.iter()) {
        total += type_score(it, a, p.ty.as_deref())?;
    }
    // prefer exact arity, then no varargs
    let penalty = if def.rest.is_some() { -2 } else { 0 } - (n as i32 - args.len() as i32).max(0);
    Some((total, penalty))
}

impl Interp {
    /// Add `new` to the overloads of `existing`; returns the dispatcher function.
    pub fn add_overload(&mut self, existing: &V, new: V) -> V {
        if let V::Func(c) = existing {
            if let Some(V::Arr(a)) = c.props.borrow().get("%overloads") {
                a.borrow_mut().push(new);
                return existing.clone();
            }
            let disp = new_closure(Closure {
                def: c.def.clone(),
                env: c.env.clone(),
                props: RefCell::new(PropMap::default()),
                parent: c.parent.clone(),
                home: RefCell::new(c.home.borrow().clone()),
                flavor: c.flavor,
            });
            disp.props.borrow_mut().set("%overloads", V::arr(vec![existing.clone(), new]));
            return V::Func(disp);
        }
        new
    }

    /// The overload to call for `args` (None when no candidate applies).
    pub fn select_overload(&self, cands: &[V], args: &[V]) -> Option<Rc<Closure>> {
        let mut best: Option<(Rc<Closure>, (u32, i32))> = None;
        for c in cands {
            let V::Func(f) = c else { continue };
            if let Some(s) = candidate_score(self, &f.def, args) {
                if best.as_ref().is_none_or(|(_, b)| s > *b) {
                    best = Some((f.clone(), s));
                }
            }
        }
        best.map(|(f, _)| f)
    }

    /// The constructor to run for `args` (by arity and argument types).
    pub fn select_ctor(&self, ctors: &[std::sync::Arc<crate::ast::FuncDef>], args: &[V]) -> Option<std::sync::Arc<crate::ast::FuncDef>> {
        let mut best: Option<(std::sync::Arc<crate::ast::FuncDef>, (u32, i32))> = None;
        for f in ctors {
            if let Some(s) = candidate_score(self, f, args) {
                if best.as_ref().is_none_or(|(_, b)| s > *b) {
                    best = Some((f.clone(), s));
                }
            }
        }
        best.map(|(f, _)| f)
    }

    /// Error for a call that matches no overload.
    pub fn no_overload<X>(&self, c: &Closure, args: &[V], pos: u32) -> crate::interp::R<X> {
        let name = c.def.name.clone().unwrap_or_default();
        let types = args.iter().map(crate::builtins::java_type).collect::<Vec<_>>().join(", ");
        let ename = if self.d == crate::ast::Dialect::Java { "NoSuchMethodException" } else { "MissingMethodException" };
        self.throw(ename, format!("No signature of method: {}() is applicable for argument types: ({})", name, types), Some(pos))
    }
}
