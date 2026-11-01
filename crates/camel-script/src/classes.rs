//! Classes for JavaScript, Groovy and Python: construction, fields / properties, accessors,
//! inheritance, `super`, Groovy traits, AST-transform annotations and metaclass methods, and
//! Python special methods.
//!
//! A class is a closure value (the constructor) with flavor [`Flavor::Class`]; its `prototype`
//! object holds the methods (Python: also the class attributes) and links to the parent
//! class's prototype. Instances are objects whose prototype is the class prototype.

use std::rc::Rc;
use std::sync::Arc;

use crate::ast::*;
use crate::interp::{new_scope, Ctrl, Env, Interp, R};
use crate::value::*;

/// Definition and environment of a class, by constructor closure address.
pub struct ClassInfo {
    pub def: Arc<ClassDef>,
    pub env: Env,
    /// Builtin exception type this class derives from (instances are error objects).
    pub error_base: Option<String>,
    /// Field initializers inherited from mixed-in traits.
    pub mixin_fields: Vec<(ClassField, Env)>,
}

fn accessor(get: Option<V>, set: Option<V>, existing: Option<V>) -> V {
    let mut p = PropMap::default();
    if let Some(V::Obj(o)) = &existing {
        if o.borrow().kind == ObjKind::Accessor {
            p = o.borrow().props.clone();
        }
    }
    if let Some(g) = get {
        p.set("get", g);
    }
    if let Some(s) = set {
        p.set("set", s);
    }
    V::obj_kind(p, ObjKind::Accessor)
}

/// Capitalize the first letter (Groovy bean property methods).
pub fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The class (constructor closure) of an instance.
pub fn class_of(v: &V) -> Option<Rc<Closure>> {
    let V::Obj(o) = v else { return None };
    let proto = o.borrow().proto.clone()?;
    let c = proto.borrow().props.get("constructor").cloned();
    match c {
        Some(V::Func(f)) if f.flavor == Flavor::Class => Some(f),
        _ => None,
    }
}

fn func_name(c: &Closure) -> String {
    match c.props.borrow().get("name") {
        Some(V::Str(s)) => s.to_string(),
        _ => c.def.name.clone().unwrap_or_default(),
    }
}

impl Interp {
    /// Evaluate a class definition.
    pub fn make_class(&mut self, c: &Arc<ClassDef>, env: &Env) -> R<V> {
        self.track_env_pub(env);
        let mut error_base = None;
        let parent = match &c.extends {
            Some(e) => {
                let p = match e {
                    Expr::Ident(n, _) if env.lookup(n).is_none() => crate::builtins::type_ns(n).unwrap_or_else(|| V::host(Host::Type(n.clone()))),
                    Expr::Type(n) => self.eval(&Expr::Type(n.clone()), env)?,
                    other => self.eval(other, env)?,
                };
                match &p {
                    V::Host(h) => {
                        let n = match &**h {
                            Host::Type(n) => Some(n.clone()),
                            Host::Ns(n) => Some(n.to_string()),
                            _ => None,
                        };
                        if let Some(n) = n.filter(|n| is_error_type(n)) {
                            error_base = Some(n);
                        }
                    }
                    V::Func(pc) => {
                        if let Some(info) = self.classes.get(&(Rc::as_ptr(pc) as usize)) {
                            error_base = info.error_base.clone();
                        }
                    }
                    _ => {}
                }
                Some(p)
            }
            None => None,
        };
        let proto = new_obj_ref(Obj::new());
        if let Some(V::Func(pc)) = &parent {
            if let Some(V::Obj(pp)) = self.func_prototype(pc) {
                proto.borrow_mut().proto = Some(pp);
            }
        }
        let ctor_def = c.ctors.first().cloned().unwrap_or_else(|| Arc::new(FuncDef::new(c.name.clone(), vec![], None, FuncBody::Block(vec![]))));
        let closure = new_closure(Closure { def: ctor_def, env: env.clone(), props: RefCell::new(PropMap::default()), parent: parent.clone(), home: RefCell::new(Some(proto.clone())), flavor: Flavor::Class });
        let class_v = V::Func(closure.clone());
        proto.borrow_mut().props.set("constructor", class_v.clone());
        closure.props.borrow_mut().set("prototype", V::Obj(proto.clone()));
        if let Some(n) = &c.name {
            closure.props.borrow_mut().set("name", V::str(n));
        }
        let py = c.style == ClassStyle::Python;
        // class scope: methods see the class name
        let cenv = new_scope(Some(env.clone()), false);
        if let Some(n) = &c.name {
            cenv.define(n, class_v.clone());
        }
        self.track_env_pub(&cenv);
        for m in &c.methods {
            let mname = match &m.key {
                Some(k) => {
                    let kv = self.eval(k, env)?;
                    self.key_string(&kv)
                }
                None => m.name.clone(),
            };
            let m = &ClassMember { name: mname, key: None, func: m.func.clone(), kind: m.kind, is_static: m.is_static };
            let flavor = match (py, m.kind, m.is_static) {
                (true, MemberKind::ClassMethod, _) => Flavor::PyClassMethod,
                (true, _, true) => Flavor::PyStatic,
                (true, _, false) => Flavor::PyMethod,
                _ => Flavor::Plain,
            };
            let f = V::Func(new_closure(Closure {
                def: m.func.clone(),
                env: cenv.clone(),
                props: RefCell::new(PropMap::default()),
                parent: parent.clone(),
                home: RefCell::new(Some(proto.clone())),
                flavor,
            }));
            let on_class = m.is_static && !py;
            match m.kind {
                MemberKind::Getter | MemberKind::Setter => {
                    let existing = if on_class { closure.props.borrow().get(&m.name).cloned() } else { proto.borrow().props.get(&m.name).cloned() };
                    let acc = if m.kind == MemberKind::Getter { accessor(Some(f), None, existing) } else { accessor(None, Some(f), existing) };
                    if on_class {
                        closure.props.borrow_mut().set(&m.name, acc);
                    } else {
                        proto.borrow_mut().props.set(&m.name, acc);
                    }
                }
                _ => {
                    if on_class {
                        closure.props.borrow_mut().set(&m.name, f);
                    } else {
                        proto.borrow_mut().props.set(&m.name, f);
                    }
                }
            }
        }
        // traits / interfaces with default methods
        let mut mixin_fields = Vec::new();
        for mx in &c.mixins {
            let t = match mx {
                Expr::Ident(n, _) => env.lookup(n),
                other => self.eval(other, env).ok(),
            };
            if let Some(V::Func(tc)) = t {
                if let Some(V::Obj(tp)) = self.func_prototype(&tc) {
                    let items: Vec<(Rc<str>, V)> = tp.borrow().props.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                    for (k, v) in items {
                        if &*k != "constructor" && proto.borrow().lookup(&k).is_none() {
                            proto.borrow_mut().props.set(&k, v);
                        }
                    }
                }
                if let Some(info) = self.classes.get(&(Rc::as_ptr(&tc) as usize)) {
                    for f in info.def.fields.iter().filter(|f| !f.is_static) {
                        mixin_fields.push((f.clone(), info.env.clone()));
                    }
                }
            }
        }
        if !c.annotations.is_empty() {
            closure.props.borrow_mut().set("%annotations", V::arr(c.annotations.iter().map(|a| V::str(a)).collect()));
        }
        self.classes.insert(Rc::as_ptr(&closure) as usize, ClassInfo { def: c.clone(), env: cenv.clone(), error_base, mixin_fields });
        // static fields / Python class attributes
        for f in c.fields.iter().filter(|f| f.is_static) {
            let scope = new_scope(Some(cenv.clone()), true);
            scope.define("this", class_v.clone());
            let v = match &f.init {
                Some(e) => self.eval(e, &scope)?,
                None => self.undef(),
            };
            if py {
                proto.borrow_mut().props.set(&f.name, v);
            } else {
                closure.props.borrow_mut().set(&f.name, v);
            }
        }
        Ok(class_v)
    }

    /// The chain of classes from the root to `c`.
    fn class_chain(&self, c: &Rc<Closure>) -> Vec<Rc<Closure>> {
        let mut chain = vec![c.clone()];
        let mut cur = c.parent.clone();
        while let Some(V::Func(p)) = cur {
            if p.flavor != Flavor::Class || chain.len() > 64 {
                break;
            }
            cur = p.parent.clone();
            chain.push(p);
        }
        chain.reverse();
        chain
    }

    fn init_fields(&mut self, c: &Rc<Closure>, this: &V) -> R<()> {
        for k in self.class_chain(c) {
            let Some((fields, env)) = self.classes.get(&(Rc::as_ptr(&k) as usize)).map(|i| {
                let mut f: Vec<(ClassField, Env)> = i.mixin_fields.clone();
                f.extend(i.def.fields.iter().filter(|f| !f.is_static).map(|f| (f.clone(), i.env.clone())));
                (f, i.env.clone())
            }) else {
                continue;
            };
            let _ = env;
            for (f, fenv) in fields {
                let scope = new_scope(Some(fenv.clone()), true);
                scope.define("this", this.clone());
                let v = match &f.init {
                    Some(e) => self.eval(e, &scope)?,
                    None => self.undef(),
                };
                self.set_member_raw(this, &f.name, v);
            }
        }
        Ok(())
    }

    fn set_member_raw(&mut self, o: &V, name: &str, v: V) {
        if let V::Obj(ob) = o {
            ob.borrow_mut().props.set(name, v);
        }
    }

    /// `new C(args)` / Python `C(args)` for a class closure.
    pub fn construct_class(&mut self, c: &Rc<Closure>, args: Vec<V>, pos: u32) -> R<V> {
        let info = self.classes.get(&(Rc::as_ptr(c) as usize)).map(|i| (i.def.clone(), i.error_base.clone()));
        let Some((def, error_base)) = info else {
            return self.construct_plain(c, args, pos);
        };
        let obj = new_obj_ref(Obj::new());
        if let Some(V::Obj(p)) = self.func_prototype(c) {
            obj.borrow_mut().proto = Some(p);
        }
        let this = V::Obj(obj.clone());
        if error_base.is_some() {
            let mut b = obj.borrow_mut();
            b.kind = ObjKind::Error;
            b.props.set("name", V::str(def.name.as_deref().unwrap_or("Error")));
            b.props.set("message", V::str(""));
        }
        self.init_fields(c, &this)?;
        match def.style {
            ClassStyle::Python => {
                let init = obj.borrow().lookup("__init__");
                match init {
                    Some(f @ V::Func(_)) => {
                        let mut a = vec![this.clone()];
                        a.extend(args);
                        self.call(&f, V::Undef, a, pos)?;
                    }
                    _ => {
                        if error_base.is_some() {
                            self.set_error_args(&this, &args);
                        } else if !args.is_empty() && !matches!(args.as_slice(), [V::Obj(k)] if k.borrow().kind == ObjKind::Kwargs) {
                            return self.throw("TypeError", format!("{}() takes no arguments", def.name.as_deref().unwrap_or("object")), Some(pos));
                        }
                    }
                }
            }
            ClassStyle::Js => {
                let has_ctor = !def.ctors.is_empty();
                if has_ctor {
                    let r = self.call_closure_as_ctor(c, this.clone(), args, pos)?;
                    if matches!(r, V::Obj(_) | V::Arr(_)) && !r.same(&this) {
                        return Ok(r);
                    }
                } else {
                    self.call_parent_ctor(c, &this, args, pos)?;
                }
            }
            ClassStyle::Groovy => {
                self.groovy_construct(c, &def, &this, args, pos)?;
            }
        }
        Ok(this)
    }

    fn set_error_args(&mut self, this: &V, args: &[V]) {
        let msg = if args.is_empty() { String::new() } else { args.iter().map(|a| self.display(a)).collect::<Vec<_>>().join(", ") };
        if let V::Obj(o) = this {
            let mut b = o.borrow_mut();
            b.props.set("message", V::string(msg));
            b.props.set("args", V::arr(args.to_vec()));
        }
    }

    fn construct_plain(&mut self, c: &Rc<Closure>, args: Vec<V>, pos: u32) -> R<V> {
        let obj = new_obj_ref(Obj::new());
        if let Some(V::Obj(p)) = self.func_prototype(c) {
            obj.borrow_mut().proto = Some(p);
        }
        let this = V::Obj(obj);
        let r = self.call_closure_as_ctor(c, this.clone(), args, pos)?;
        Ok(if matches!(r, V::Obj(_) | V::Arr(_)) { r } else { this })
    }

    /// Call the parent constructor of class `c` on `this` (implicit / `super(...)`).
    pub fn call_parent_ctor(&mut self, c: &Rc<Closure>, this: &V, args: Vec<V>, pos: u32) -> R<()> {
        match &c.parent {
            Some(V::Func(p)) => {
                let p = p.clone();
                self.invoke_ctor_on(&p, this, args, pos)
            }
            Some(V::Host(h)) => {
                if let Host::Type(_) | Host::Ns(_) = &**h {
                    self.set_error_args(this, &args);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Run the constructor of class `k` (not allocating) on `this`.
    pub fn invoke_ctor_on(&mut self, k: &Rc<Closure>, this: &V, args: Vec<V>, pos: u32) -> R<()> {
        let info = self.classes.get(&(Rc::as_ptr(k) as usize)).map(|i| i.def.clone());
        match info {
            Some(def) => match def.style {
                ClassStyle::Js => {
                    if def.ctors.is_empty() {
                        self.call_parent_ctor(k, this, args, pos)
                    } else {
                        self.call_closure_as_ctor(k, this.clone(), args, pos).map(|_| ())
                    }
                }
                ClassStyle::Groovy => self.groovy_construct(k, &def, this, args, pos),
                ClassStyle::Python => {
                    let init = def.methods.iter().find(|m| m.name == "__init__").map(|m| m.func.clone());
                    let _ = init;
                    let proto = self.func_prototype(k);
                    let f = match proto {
                        Some(V::Obj(p)) => p.borrow().lookup("__init__"),
                        _ => None,
                    };
                    if let Some(f) = f {
                        let mut a = vec![this.clone()];
                        a.extend(args);
                        self.call(&f, V::Undef, a, pos)?;
                    }
                    Ok(())
                }
            },
            None => self.call_closure_as_ctor(k, this.clone(), args, pos).map(|_| ()),
        }
    }

    fn groovy_construct(&mut self, c: &Rc<Closure>, def: &Arc<ClassDef>, this: &V, args: Vec<V>, pos: u32) -> R<()> {
        let named = matches!(args.as_slice(), [V::Obj(o)] if o.borrow().kind == ObjKind::Plain && o.borrow().proto.is_none());
        let ctor = def.ctors.iter().find(|f| f.params.len() == args.len() || (f.rest.is_some() && args.len() >= f.params.len())).cloned();
        match ctor {
            Some(f) => {
                // implicit super(): call the parent's no-arg constructor unless the body calls super(...)
                let calls_super = matches!(&f.body, FuncBody::Block(b) if matches!(b.first(), Some(Stmt::Expr(Expr::Call { callee, .. })) if matches!(**callee, Expr::Super(_) | Expr::This)));
                if !calls_super {
                    self.call_parent_ctor(c, this, vec![], pos)?;
                }
                let k = new_closure(Closure { def: f, env: c.env.clone(), props: RefCell::new(PropMap::default()), parent: c.parent.clone(), home: RefCell::new(c.home.borrow().clone()), flavor: Flavor::Plain });
                self.call_closure_as_ctor(&k, this.clone(), args, pos)?;
                Ok(())
            }
            None if args.is_empty() => self.call_parent_ctor(c, this, vec![], pos),
            None if named => {
                self.call_parent_ctor(c, this, vec![], pos)?;
                let items: Vec<(Rc<str>, V)> = match &args[0] {
                    V::Obj(o) => o.borrow().props.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                    _ => vec![],
                };
                for (k, v) in items {
                    self.set_member(this, &k, v, pos)?;
                }
                Ok(())
            }
            None => {
                let tuple = def.annotations.iter().any(|a| matches!(a.as_str(), "TupleConstructor" | "Canonical" | "Immutable"));
                if tuple {
                    let names: Vec<String> = self.class_chain(c).iter().filter_map(|k| self.classes.get(&(Rc::as_ptr(k) as usize))).flat_map(|i| i.def.fields.iter().filter(|f| !f.is_static).map(|f| f.name.clone()).collect::<Vec<_>>()).collect();
                    if args.len() <= names.len() {
                        for (n, v) in names.iter().zip(args) {
                            self.set_member_raw(this, n, v);
                        }
                        return Ok(());
                    }
                }
                if def.extends.is_some() && def.ctors.is_empty() {
                    return self.call_parent_ctor(c, this, args, pos);
                }
                let types = args.iter().map(crate::builtins::java_type).collect::<Vec<_>>().join(", ");
                self.throw("GroovyRuntimeException", format!("Could not find matching constructor for: {}({})", def.name.as_deref().unwrap_or("Object"), types), Some(pos))
            }
        }
    }

    /// A user-defined method of an instance's class (`toString`, `__str__`, `equals`...).
    pub fn user_method(&self, v: &V, name: &str) -> Option<V> {
        let V::Obj(o) = v else { return None };
        let b = o.borrow();
        let proto = b.proto.clone()?;
        drop(b);
        let f = proto.borrow().lookup(name)?;
        matches!(f, V::Func(_)).then_some(f)
    }

    /// Call a user method of an instance (binding `self` for Python methods).
    pub fn call_user_method(&mut self, recv: &V, f: &V, args: Vec<V>, pos: u32) -> R<V> {
        if let V::Func(c) = f {
            match c.flavor {
                Flavor::PyMethod => {
                    let mut a = vec![recv.clone()];
                    a.extend(args);
                    return self.call(f, V::Undef, a, pos);
                }
                Flavor::PyClassMethod => {
                    let cls = class_of(recv).map(V::Func).unwrap_or_else(|| recv.clone());
                    let mut a = vec![cls];
                    a.extend(args);
                    return self.call(f, V::Undef, a, pos);
                }
                _ => {}
            }
        }
        self.call(f, recv.clone(), args, pos)
    }

    /// String conversion honoring user-defined `toString()` / `__str__`.
    pub fn to_str(&mut self, v: &V) -> R<String> {
        if let V::Obj(o) = v {
            if o.borrow().proto.is_some() {
                let name = match self.d {
                    Dialect::Python => "__str__",
                    _ => "toString",
                };
                let f = self.user_method(v, name).or_else(|| if self.d == Dialect::Python { self.user_method(v, "__repr__") } else { None });
                if let Some(f) = f {
                    let r = self.call_user_method(v, &f, vec![], 0)?;
                    return Ok(self.display(&r));
                }
                if self.d == Dialect::Groovy {
                    if let Some(s) = self.groovy_tostring(v)? {
                        return Ok(s);
                    }
                }
            }
        }
        Ok(self.display(v))
    }

    /// Default string form of a Groovy instance (`@ToString` / `@Canonical`: `Name(a, b)`).
    fn groovy_tostring(&mut self, v: &V) -> R<Option<String>> {
        let Some(c) = class_of(v) else { return Ok(None) };
        let annotated = matches!(c.props.borrow().get("%annotations"), Some(V::Arr(a)) if a.borrow().iter().any(|x| matches!(x, V::Str(s) if matches!(&**s, "ToString" | "Canonical" | "Immutable"))));
        let name = func_name(&c);
        if !annotated {
            return Ok(Some(format!("{}@{:x}", name, (Rc::as_ptr(&c) as usize) & 0xffffff)));
        }
        let mut parts = Vec::new();
        if let V::Obj(o) = v {
            let vals: Vec<V> = o.borrow().props.values().cloned().collect();
            for x in vals {
                parts.push(self.to_str(&x)?);
            }
        }
        Ok(Some(format!("{}({})", name, parts.join(", "))))
    }

    /// `==` with user-defined equality (`__eq__`, Groovy `equals` / `@EqualsAndHashCode`).
    pub fn user_equals(&mut self, a: &V, b: &V, pos: u32) -> R<Option<bool>> {
        let name = if self.d == Dialect::Python { "__eq__" } else { "equals" };
        if let Some(f) = self.user_method(a, name) {
            let r = self.call_user_method(a, &f, vec![b.clone()], pos)?;
            return Ok(Some(self.truthy(&r)));
        }
        if self.d == Dialect::Groovy {
            if let (Some(ca), Some(cb)) = (class_of(a), class_of(b)) {
                if Rc::ptr_eq(&ca, &cb) {
                    let annotated = matches!(ca.props.borrow().get("%annotations"), Some(V::Arr(x)) if x.borrow().iter().any(|x| matches!(x, V::Str(s) if matches!(&**s, "EqualsAndHashCode" | "Canonical" | "Immutable"))));
                    if annotated {
                        return Ok(Some(crate::interp::deep_eq(a, b)));
                    }
                }
                return Ok(Some(a.same(b)));
            }
        }
        if class_of(a).is_some() || class_of(b).is_some() {
            return Ok(Some(a.same(b)));
        }
        Ok(None)
    }

    /// Binary operator overloading (Python `__add__`..., Groovy `plus`/`minus`/...).
    pub fn user_binary(&mut self, op: BinOp, a: &V, b: &V, pos: u32) -> R<Option<V>> {
        if class_of(a).is_none() {
            return Ok(None);
        }
        let name = match (self.d, op) {
            (Dialect::Python, BinOp::Add) => "__add__",
            (Dialect::Python, BinOp::Sub) => "__sub__",
            (Dialect::Python, BinOp::Mul) => "__mul__",
            (Dialect::Python, BinOp::Div) => "__truediv__",
            (Dialect::Python, BinOp::Lt) => "__lt__",
            (Dialect::Python, BinOp::Le) => "__le__",
            (Dialect::Python, BinOp::Gt) => "__gt__",
            (Dialect::Python, BinOp::Ge) => "__ge__",
            (Dialect::Groovy, BinOp::Add) => "plus",
            (Dialect::Groovy, BinOp::Sub) => "minus",
            (Dialect::Groovy, BinOp::Mul) => "multiply",
            (Dialect::Groovy, BinOp::Div) => "div",
            (Dialect::Groovy, BinOp::Shl) => "leftShift",
            (Dialect::Groovy, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Cmp) => "compareTo",
            _ => return Ok(None),
        };
        let Some(f) = self.user_method(a, name) else { return Ok(None) };
        let r = self.call_user_method(a, &f, vec![b.clone()], pos)?;
        if name == "compareTo" && op != BinOp::Cmp {
            let n = r.as_f64().unwrap_or(0.0);
            return Ok(Some(V::Bool(match op {
                BinOp::Lt => n < 0.0,
                BinOp::Le => n <= 0.0,
                BinOp::Gt => n > 0.0,
                _ => n >= 0.0,
            })));
        }
        Ok(Some(r))
    }

    /// Groovy property read on an instance (getters, fields, statics, `propertyMissing`).
    pub fn groovy_get_property(&mut self, recv: &V, o: &ObjRef, name: &str, pos: u32) -> R<Option<V>> {
        let Some(c) = class_of(recv) else { return Ok(None) };
        if name == "class" {
            return Ok(Some(V::Func(c)));
        }
        if let Some(g) = self.user_method(recv, &format!("get{}", cap(name))) {
            return self.call(&g, recv.clone(), vec![], pos).map(Some);
        }
        if let Some(v) = o.borrow().props.get(name).cloned() {
            return Ok(Some(v));
        }
        if let Some(g) = self.user_method(recv, &format!("is{}", cap(name))) {
            return self.call(&g, recv.clone(), vec![], pos).map(Some);
        }
        if !is_class_meta(name) {
            for k in self.class_chain(&c).iter().rev() {
                if let Some(v) = k.props.borrow().get(name).cloned() {
                    return Ok(Some(v));
                }
            }
        }
        if let Some(f) = o.borrow().lookup(name) {
            if matches!(f, V::Func(_)) {
                return Ok(Some(V::Method(Rc::new((recv.clone(), name.to_string())))));
            }
        }
        if let Some(pm) = self.user_method(recv, "propertyMissing") {
            return self.call(&pm, recv.clone(), vec![V::str(name)], pos).map(Some);
        }
        if name == "properties" {
            return Ok(Some(self.groovy_properties(o)));
        }
        if self.d == Dialect::Groovy && o.borrow().lookup(name).is_none() && self.meta_method(recv, name).is_none() {
            return self.throw("MissingPropertyException", format!("No such property: {} for class: {}", name, func_name(&c)), Some(pos)).map(Some);
        }
        Ok(None)
    }

    /// Groovy `obj.properties`: the instance fields (and `class`).
    fn groovy_properties(&self, o: &ObjRef) -> V {
        let mut m = Obj::new();
        for (k, v) in o.borrow().props.iter() {
            if !k.starts_with('%') && !k.starts_with('$') {
                m.props.set(k, v.clone());
            }
        }
        V::Obj(new_obj_ref(m))
    }

    /// Groovy method fallback on an instance: bean accessors, `methodMissing`, metaclass methods.
    pub fn groovy_method_fallback(&mut self, recv: &V, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
        if let Some(m) = self.meta_method(recv, name) {
            return self.call(&m, recv.clone(), args.to_vec(), pos).map(Some);
        }
        let V::Obj(o) = recv else { return Ok(None) };
        let Some(c) = class_of(recv) else { return Ok(None) };
        match (name, args.len()) {
            ("getClass", 0) => return Ok(Some(V::Func(c))),
            ("toString", 0) => {
                let s = self.to_str(recv)?;
                return Ok(Some(V::string(s)));
            }
            ("getProperties", 0) => return Ok(Some(self.groovy_properties(o))),
            ("hasProperty", 1) => {
                let n = self.display(&args[0]);
                let has = o.borrow().props.contains(&n) || self.user_method(recv, &format!("get{}", cap(&n))).is_some();
                return Ok(Some(if has {
                    let mut m = Obj::new();
                    m.props.set("name", V::string(n));
                    V::Obj(new_obj_ref(m))
                } else {
                    V::Null
                }));
            }
            ("respondsTo", 1) => {
                let n = self.display(&args[0]);
                let has = self.user_method(recv, &n).is_some();
                return Ok(Some(V::arr(if has { vec![V::string(n)] } else { vec![] })));
            }
            ("hashCode", 0) => return Ok(Some(V::Int((Rc::as_ptr(o) as usize as i64) & 0x7fff_ffff))),
            _ => {}
        }
        if let Some(p) = name.strip_prefix("get").filter(|p| !p.is_empty() && args.is_empty()) {
            let prop = crate::host::lc_first_pub(p);
            if let Some(v) = o.borrow().props.get(&prop).cloned() {
                return Ok(Some(v));
            }
        }
        if let Some(p) = name.strip_prefix("is").filter(|p| !p.is_empty() && args.is_empty()) {
            let prop = crate::host::lc_first_pub(p);
            if let Some(v) = o.borrow().props.get(&prop).cloned() {
                return Ok(Some(v));
            }
        }
        if let Some(p) = name.strip_prefix("set").filter(|p| !p.is_empty() && args.len() == 1) {
            let prop = crate::host::lc_first_pub(p);
            o.borrow_mut().props.set(&prop, args[0].clone());
            return Ok(Some(V::Null));
        }
        if let Some(mm) = self.user_method(recv, "methodMissing") {
            return self.call(&mm, recv.clone(), vec![V::str(name), V::arr(args.to_vec())], pos).map(Some);
        }
        Ok(None)
    }

    /// A metaclass method (`Type.metaClass.name = { ... }`) applicable to `recv`.
    pub fn meta_method(&self, recv: &V, name: &str) -> Option<V> {
        if self.meta_methods.is_empty() {
            return None;
        }
        let mut types = Vec::new();
        if let Some(c) = class_of(recv) {
            types.push(func_name(&c));
        }
        let jt = crate::builtins::java_type(recv);
        types.push(jt.rsplit('.').next().unwrap_or(&jt).to_string());
        if matches!(recv, V::Arr(_)) {
            types.push("List".into());
        }
        if matches!(recv, V::Obj(_)) {
            types.push("Map".into());
        }
        if recv.is_number() {
            types.push("Number".into());
        }
        types.push("Object".into());
        types.iter().find_map(|t| self.meta_methods.get(t).and_then(|m| m.get(name).cloned()))
    }

    /// Lookup of a bare identifier inside Groovy / Java methods: locals first, then the fields
    /// and statics of `this`, then outer scopes.
    pub fn lookup_with_fields(&self, name: &str, env: &Env) -> Option<V> {
        let mut s = Some(env.clone());
        while let Some(sc) = s {
            if let Some(v) = sc.vars.borrow().get(name) {
                return Some(v.clone());
            }
            if sc.func && sc.vars.borrow().contains_key("%home") {
                let this = sc.vars.borrow().get("this").cloned();
                if let Some(this @ V::Obj(_)) = this.clone() {
                    let V::Obj(o) = &this else { unreachable!() };
                    if let Some(v) = o.borrow().props.get(name) {
                        return Some(v.clone());
                    }
                    if let Some(c) = class_of(&this) {
                        if !is_class_meta(name) {
                            for k in self.class_chain(&c).iter().rev() {
                                if let Some(v) = k.props.borrow().get(name) {
                                    return Some(v.clone());
                                }
                            }
                        }
                    }
                }
                if let Some(V::Func(c)) = this {
                    // static context: the statics of the class chain
                    if c.flavor == Flavor::Class && !is_class_meta(name) {
                        for k in self.class_chain(&c).iter().rev() {
                            if let Some(v) = k.props.borrow().get(name) {
                                if !v.is_callable() {
                                    return Some(v.clone());
                                }
                            }
                        }
                    }
                }
            }
            if sc.func && self.d == Dialect::Groovy {
                // closures run by `with` / `tap` / metaclass methods: the delegate's properties
                let del = sc.vars.borrow().get("delegate").cloned();
                if let Some(d @ V::Obj(_)) = del {
                    if class_of(&d).is_some() {
                        let V::Obj(o) = &d else { unreachable!() };
                        if let Some(v) = o.borrow().props.get(name) {
                            return Some(v.clone());
                        }
                    }
                }
            }
            s = sc.parent.clone();
        }
        None
    }

    /// Assignment to a bare identifier in Groovy / Java methods (fields of `this`).
    pub fn assign_with_fields(&self, name: &str, v: &V, env: &Env) -> bool {
        let mut s = Some(env.clone());
        while let Some(sc) = s {
            if let Some(slot) = sc.vars.borrow_mut().get_mut(name) {
                *slot = v.clone();
                return true;
            }
            if sc.func && sc.vars.borrow().contains_key("%home") {
                let this = sc.vars.borrow().get("this").cloned();
                if let Some(this @ V::Obj(_)) = this {
                    let V::Obj(o) = &this else { unreachable!() };
                    if o.borrow().props.contains(name) {
                        o.borrow_mut().props.set(name, v.clone());
                        return true;
                    }
                    if let Some(c) = class_of(&this) {
                        for k in self.class_chain(&c).iter().rev() {
                            if k.props.borrow().contains(name) {
                                k.props.borrow_mut().set(name, v.clone());
                                return true;
                            }
                        }
                    }
                }
            }
            s = sc.parent.clone();
        }
        false
    }

    /// Accessor (getter/setter pair) of a property, searching the prototype chain.
    pub fn find_accessor(o: &ObjRef, name: &str) -> Option<ObjRef> {
        let mut cur = Some(o.clone());
        let mut n = 0;
        while let Some(c) = cur {
            let b = c.borrow();
            if let Some(V::Obj(a)) = b.props.get(name) {
                if a.borrow().kind == ObjKind::Accessor {
                    return Some(a.clone());
                }
                return None;
            }
            if b.props.contains(name) {
                return None;
            }
            cur = b.proto.clone();
            n += 1;
            if n > 256 {
                break;
            }
        }
        None
    }

    /// Call the getter of an accessor.
    pub fn call_getter(&mut self, acc: &ObjRef, this: &V, pos: u32) -> R<V> {
        let g = acc.borrow().props.get("get").cloned();
        match g {
            Some(f) => self.call_user_method(this, &f, vec![], pos),
            None => Ok(self.undef()),
        }
    }

    /// Call the setter of an accessor (ignored without setter, like non-strict JavaScript;
    /// Python raises `AttributeError`).
    pub fn call_setter(&mut self, acc: &ObjRef, this: &V, name: &str, v: V, pos: u32) -> R<()> {
        let s = acc.borrow().props.get("set").cloned();
        match s {
            Some(f) => self.call_user_method(this, &f, vec![v], pos).map(|_| ()),
            None if self.d == Dialect::Python => self.throw("AttributeError", format!("can't set attribute '{}'", name), Some(pos)),
            None => Ok(()),
        }
    }

    /// Accessor for object literals / `Object.defineProperty`.
    pub fn make_accessor(&self, get: Option<V>, set: Option<V>, existing: Option<V>) -> V {
        accessor(get, set, existing)
    }

    /// Python special method dispatch for an instance (`__len__`, `__getitem__`, ...).
    pub fn py_special(&mut self, recv: &V, name: &str, args: Vec<V>, pos: u32) -> R<Option<V>> {
        if self.d != Dialect::Python || class_of(recv).is_none() {
            return Ok(None);
        }
        match self.user_method(recv, name) {
            Some(f) => self.call_user_method(recv, &f, args, pos).map(Some),
            None => Ok(None),
        }
    }

    /// Python `super()` method call target: the method `name` above the current class.
    pub fn super_method(&mut self, env: &Env, name: &str, pos: u32) -> R<(V, V)> {
        let this = env.lookup("this").or_else(|| env.lookup("self")).unwrap_or(V::Undef);
        let this = match (&this, self.d) {
            (V::Undef, Dialect::Python) => self.first_param(env).unwrap_or(V::Undef),
            _ => this,
        };
        let home = match env.lookup("%home") {
            Some(V::Obj(h)) => h,
            _ => return self.throw("SyntaxError", "'super' keyword unexpected here".into(), Some(pos)),
        };
        let parent_proto = home.borrow().proto.clone();
        let f = parent_proto.as_ref().and_then(|p| p.borrow().lookup(name));
        match f {
            Some(f) => Ok((f, this)),
            None => {
                // a builtin base: Exception.__init__ / Groovy super.toString()...
                Ok((V::Method(Rc::new((this.clone(), format!("%super:{}", name)))), this))
            }
        }
    }

    fn first_param(&self, env: &Env) -> Option<V> {
        env.lookup("%self")
    }

    /// Builtin base class methods reached through `super` (`Exception.__init__`, `toString`).
    pub fn super_builtin(&mut self, recv: &V, name: &str, args: &[V], pos: u32) -> R<V> {
        match name {
            "__init__" | "<init>" => {
                self.set_error_args(recv, args);
                Ok(V::Null)
            }
            "toString" | "__str__" | "__repr__" => Ok(V::string(self.display(recv))),
            "equals" | "__eq__" => Ok(V::Bool(recv.same(args.first().unwrap_or(&V::Null)))),
            "hashCode" | "__hash__" => Ok(V::Int(0)),
            _ => match self.call_method(recv, name, args.to_vec(), pos) {
                Err(Ctrl::Throw(e)) => Err(Ctrl::Throw(e)),
                other => other,
            },
        }
    }
}

/// Properties of a class closure that are not static members.
fn is_class_meta(n: &str) -> bool {
    matches!(n, "name" | "prototype" | "length") || n.starts_with('%')
}

fn is_error_type(n: &str) -> bool {
    n.ends_with("Exception") || n.ends_with("Error") || n == "Throwable" || n == "StopIteration"
}

use std::cell::RefCell;
