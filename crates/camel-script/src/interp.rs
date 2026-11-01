//! Tree-walking interpreter over the shared AST with per-language semantics.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::ast::*;
use crate::host::HostState;
use crate::value::*;

/// A lexical scope.
pub struct Scope {
    pub vars: RefCell<HashMap<Rc<str>, V>>,
    pub parent: Option<Env>,
    /// Function scope (target of `var` declarations).
    pub func: bool,
    /// Python `global` declarations of this scope.
    pub globals: RefCell<Vec<String>>,
}

/// A scope handle.
pub type Env = Rc<Scope>;

/// Create a scope.
pub fn new_scope(parent: Option<Env>, func: bool) -> Env {
    Rc::new(Scope { vars: RefCell::new(HashMap::new()), parent, func, globals: RefCell::new(Vec::new()) })
}

impl Scope {
    pub fn define(&self, name: &str, v: V) {
        self.vars.borrow_mut().insert(Rc::from(name), v);
    }
    pub fn lookup(&self, name: &str) -> Option<V> {
        if let Some(v) = self.vars.borrow().get(name) {
            return Some(v.clone());
        }
        let mut p = self.parent.clone();
        while let Some(s) = p {
            if let Some(v) = s.vars.borrow().get(name) {
                return Some(v.clone());
            }
            p = s.parent.clone();
        }
        None
    }
    /// Assign an existing variable (walking up); false when not found.
    pub fn assign(&self, name: &str, v: V) -> bool {
        if let Some(slot) = self.vars.borrow_mut().get_mut(name) {
            *slot = v;
            return true;
        }
        let mut p = self.parent.clone();
        while let Some(s) = p {
            if let Some(slot) = s.vars.borrow_mut().get_mut(name) {
                *slot = v;
                return true;
            }
            p = s.parent.clone();
        }
        false
    }
}

/// Non-local control flow (exceptions, return, break, continue).
pub enum Ctrl {
    Throw(V),
    Return(V),
    Break(Option<String>),
    Continue(Option<String>),
    /// A generator / async function suspends at `yield` / `await` with this value.
    Yield(V),
    /// Java `yield value;` in a switch expression arm.
    Value(V),
}

/// Interpreter result.
pub type R<T> = Result<T, Ctrl>;

/// Maximum depth of nested function calls.
pub const MAX_DEPTH: usize = 900;
const MAX_STEPS: u64 = 200_000_000;

/// The interpreter.
pub struct Interp {
    pub d: Dialect,
    pub src: Arc<str>,
    pub host: Rc<HostState>,
    pub globals: Env,
    pub depth: usize,
    pub max_depth: usize,
    steps: u64,
    /// Scopes captured by closures (cleared at teardown to break reference cycles).
    closure_envs: RefCell<Vec<std::rc::Weak<Scope>>>,
    /// Replay contexts of the running generators.
    pub ystack: Vec<crate::gen::YieldCtx>,
    pub yield_cache: HashMap<usize, bool>,
    /// Exceptions being handled (Python bare `raise`).
    pub exc_stack: Vec<V>,
    /// Microtask / timer queues.
    pub queues: crate::promise::Queues,
    /// Registered and well-known symbols.
    pub symbols: HashMap<String, V>,
    pub next_symbol: u64,
    /// DataSonnet import cache.
    pub imports: HashMap<String, V>,
    pub import_asts: Vec<Rc<Program>>,
    /// Class definitions by constructor address.
    pub classes: HashMap<usize, crate::classes::ClassInfo>,
    /// Groovy metaclass methods by type name.
    pub meta_methods: HashMap<String, HashMap<String, V>>,
    /// Bean registry lookups (SpEL `@name`).
    pub beans: HashMap<String, V>,
    /// Directories of the ES modules / Python modules being loaded (relative imports).
    pub module_dirs: Vec<std::path::PathBuf>,
    /// Directory of the script itself (option `scriptDir`), for relative imports.
    pub script_dir: Option<std::path::PathBuf>,
    /// Extra properties of arrays (tagged template `raw`, regex match `index` / `groups`), by array address.
    pub arr_extra: HashMap<usize, (V, PropMap)>,
}

/// JavaScript number to string.
pub fn js_num_str(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if f == 0.0 {
        return "0".into();
    }
    if f.fract() == 0.0 && f.abs() < 1e21 {
        return format!("{}", f as i128);
    }
    let a = f.abs();
    if !(1e-7..1e21).contains(&a) {
        let s = format!("{:e}", f);
        // Rust: 1e21 / 1.5e-7 -> JS: 1e+21 / 1.5e-7
        if let Some((m, e)) = s.split_once('e') {
            if e.starts_with('-') {
                return format!("{}e{}", m, e);
            }
            return format!("{}e+{}", m, e);
        }
        return s;
    }
    format!("{}", f)
}

/// Java / Groovy / Python double to string.
pub fn java_num_str(f: f64, python: bool) -> String {
    if f.is_nan() {
        return if python { "nan".into() } else { "NaN".into() };
    }
    if f.is_infinite() {
        return match (python, f > 0.0) {
            (true, true) => "inf".into(),
            (true, false) => "-inf".into(),
            (false, true) => "Infinity".into(),
            (false, false) => "-Infinity".into(),
        };
    }
    let a = f.abs();
    if a != 0.0 && !(1e-4..1e16).contains(&a) {
        let s = format!("{:e}", f);
        if python {
            if let Some((m, e)) = s.split_once('e') {
                let m = if m.contains('.') { m.to_string() } else { m.to_string() };
                let (sign, digits) = if let Some(d) = e.strip_prefix('-') { ("-", d) } else { ("+", e) };
                return format!("{}e{}{:0>2}", m, sign, digits);
            }
        } else if let Some((m, e)) = s.split_once('e') {
            let m = if m.contains('.') { m.to_string() } else { format!("{}.0", m) };
            return format!("{}E{}", m, e);
        }
        return s;
    }
    format!("{:?}", f)
}

/// Normalize a JS / Jsonnet number: integral doubles become integers.
pub fn norm_num(f: f64) -> V {
    if f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 && !(f == 0.0 && f.is_sign_negative()) {
        V::Int(f as i64)
    } else {
        V::Num(f)
    }
}

pub(crate) enum Mode {
    Declare(VarKind),
    Assign,
}

pub(crate) fn decl_has_scope(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| matches!(s, Stmt::Var(VarKind::Let | VarKind::Const, _) | Stmt::Func(_) | Stmt::Class(_)))
}

impl Interp {
    /// Create an interpreter with the given host state.
    pub fn new(d: Dialect, src: Arc<str>, host: Rc<HostState>) -> Interp {
        let globals = new_scope(None, true);
        let mut it = Interp {
            d,
            src,
            host,
            globals,
            depth: 0,
            max_depth: MAX_DEPTH,
            steps: 0,
            closure_envs: RefCell::new(Vec::new()),
            ystack: Vec::new(),
            yield_cache: HashMap::new(),
            exc_stack: Vec::new(),
            queues: Default::default(),
            symbols: HashMap::new(),
            next_symbol: 0,
            imports: HashMap::new(),
            import_asts: Vec::new(),
            classes: HashMap::new(),
            meta_methods: HashMap::new(),
            beans: HashMap::new(),
            module_dirs: Vec::new(),
            script_dir: None,
            arr_extra: HashMap::new(),
        };
        crate::builtins::install_globals(&mut it);
        it
    }

    pub(crate) fn step_pub(&mut self) -> R<()> {
        self.step()
    }
    pub(crate) fn hoist_pub(&mut self, stmts: &[Stmt], env: &Env) {
        self.hoist(stmts, env)
    }
    pub(crate) fn exec_labeled_pub(&mut self, s: &Stmt, env: &Env, label: Option<&str>) -> R<V> {
        self.exec_labeled(s, env, label)
    }
    pub(crate) fn case_matches_pub(&mut self, dv: &V, tv: &V) -> R<bool> {
        self.case_matches(dv, tv)
    }
    pub(crate) fn catch_matches_pub(&self, types: &[String], exc: &V) -> bool {
        self.catch_matches(types, exc)
    }
    pub(crate) fn assign_pat_pub(&mut self, pat: &Pat, v: V, mode: &Mode, env: &Env) -> R<()> {
        self.assign_pat(pat, v, mode, env)
    }
    pub(crate) fn keys_of_pub(&self, v: &V) -> Vec<V> {
        self.keys_of(v)
    }
    pub(crate) fn track_env_pub(&self, env: &Env) {
        self.track_env(env)
    }
    /// Call a constructor closure with an explicit `this`.
    pub(crate) fn call_closure_as_ctor(&mut self, c: &Rc<Closure>, this: V, args: Vec<V>, pos: u32) -> R<V> {
        self.call_closure(c, this, args, pos)
    }

    fn step(&mut self) -> R<()> {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return self.throw("Error", "script exceeded the maximum number of execution steps".into(), None);
        }
        Ok(())
    }

    // ------------------------------------------------------------------ errors
    /// Build an error object.
    pub fn make_error(&self, name: &str, msg: String, pos: Option<u32>) -> V {
        let mut p = PropMap::default();
        p.set("name", V::str(name));
        p.set("message", V::string(msg));
        if let Some(pos) = pos {
            let (l, c) = crate::error::line_col(&self.src, pos as usize);
            p.set("lineNumber", V::Int(l as i64));
            p.set("columnNumber", V::Int(c as i64));
        }
        V::Obj(Rc::new(RefCell::new(Obj { props: p, proto: None, kind: ObjKind::Error })))
    }

    /// Throw an error object.
    pub fn throw<X>(&self, name: &str, msg: String, pos: Option<u32>) -> R<X> {
        Err(Ctrl::Throw(self.make_error(name, msg, pos)))
    }

    /// Throw a type error with the dialect's exception name.
    pub fn type_error<X>(&self, msg: String, pos: Option<u32>) -> R<X> {
        let name = match self.d {
            Dialect::Js => "TypeError",
            Dialect::Python => "TypeError",
            Dialect::Groovy | Dialect::Mvel | Dialect::Ognl | Dialect::Spel | Dialect::Java => "IllegalArgumentException",
            Dialect::Jsonnet => "RUNTIME ERROR",
        };
        self.throw(name, msg, pos)
    }

    fn null_error<X>(&self, what: &str, name: &str, v: &V, pos: u32) -> R<X> {
        let (ename, msg) = match self.d {
            Dialect::Js => ("TypeError", format!("Cannot {} properties of {} ({} '{}')", what, self.display(v), if what == "read" { "reading" } else { "setting" }, name)),
            Dialect::Python => ("AttributeError", format!("'NoneType' object has no attribute '{}'", name)),
            Dialect::Groovy => ("NullPointerException", format!("Cannot {} property '{}' on null object", if what == "read" { "get" } else { "set" }, name)),
            Dialect::Spel => ("SpelEvaluationException", format!("EL1007E: Property or field '{}' cannot be found on null", name)),
            Dialect::Ognl => ("OgnlException", format!("source is null for getProperty(null, \"{}\")", name)),
            Dialect::Mvel => ("NullPointerException", format!("null pointer: cannot access property '{}' of null", name)),
            Dialect::Jsonnet => ("RUNTIME ERROR", format!("field does not exist: {} (indexing null)", name)),
            Dialect::Java => ("NullPointerException", format!("Cannot {} field \"{}\" because value is null", if what == "read" { "read" } else { "assign" }, name)),
        };
        self.throw(ename, msg, Some(pos))
    }

    // ------------------------------------------------------------------ programs & statements
    /// Run a program; returns the completion value (last expression statement or `return`).
    pub fn run_program(&mut self, prog: &Program) -> R<V> {
        let env = self.globals.clone();
        self.hoist(&prog.body, &env);
        let mut last = V::Undef;
        for s in &prog.body {
            match self.exec(s, &env) {
                Ok(v) => {
                    if !matches!(s, Stmt::Func(_) | Stmt::Class(_) | Stmt::Empty) && !(matches!(s, Stmt::Var(..)) && self.d == Dialect::Js) {
                        last = v;
                    }
                }
                Err(Ctrl::Return(v)) => {
                    last = v;
                    break;
                }
                Err(Ctrl::Break(_)) | Err(Ctrl::Continue(_)) => {}
                Err(Ctrl::Yield(_)) => return self.throw("SyntaxError", "'yield' / 'await' outside of a generator or async function".into(), None),
                Err(e) => return Err(e),
            }
        }
        if self.d == Dialect::Js {
            self.drain_jobs()?;
            // the settled value of a promise result
            match crate::promise::settled(&last) {
                Some(crate::promise::PState::Fulfilled(v)) => return Ok(v),
                Some(crate::promise::PState::Rejected(e)) => return Err(Ctrl::Throw(e)),
                _ => {}
            }
        }
        Ok(last)
    }

    fn hoist(&mut self, stmts: &[Stmt], env: &Env) {
        for s in stmts {
            if let Stmt::Func(f) = s {
                if let Some(n) = &f.name {
                    let c = self.make_closure(f.clone(), env);
                    env.define(n, c);
                }
            }
        }
    }

    /// Execute a list of statements in a scope; returns the completion value.
    pub fn exec_block(&mut self, stmts: &[Stmt], env: &Env) -> R<V> {
        let env = if self.d != Dialect::Python && decl_has_scope(stmts) { new_scope(Some(env.clone()), false) } else { env.clone() };
        self.hoist(stmts, &env);
        let mut last = V::Undef;
        for s in stmts {
            let v = self.exec(s, &env)?;
            if !matches!(s, Stmt::Func(_) | Stmt::Class(_) | Stmt::Empty) && !(matches!(s, Stmt::Var(..)) && self.d == Dialect::Js) {
                last = v;
            }
        }
        Ok(last)
    }

    fn label_matches(label: &Option<String>, mine: Option<&str>) -> bool {
        match label {
            None => true,
            Some(l) => mine == Some(l.as_str()),
        }
    }

    /// Execute a statement.
    pub fn exec(&mut self, s: &Stmt, env: &Env) -> R<V> {
        self.exec_labeled(s, env, None)
    }

    fn exec_labeled(&mut self, s: &Stmt, env: &Env, label: Option<&str>) -> R<V> {
        self.step()?;
        match s {
            Stmt::Expr(e) => self.eval(e, env),
            Stmt::Var(kind, decls) => {
                let mut last = V::Undef;
                for (pat, init) in decls {
                    let v = match init {
                        Some(e) => self.eval_named(e, env, pat)?,
                        None => {
                            if let (Pat::Id(n), VarKind::Var) = (pat, kind) {
                                if self.lookup_var(n, env).is_some() {
                                    continue;
                                }
                            }
                            if self.d == Dialect::Js {
                                V::Undef
                            } else {
                                V::Null
                            }
                        }
                    };
                    last = v.clone();
                    self.assign_pat(pat, v, &Mode::Declare(*kind), env)?;
                }
                Ok(last)
            }
            Stmt::Func(f) => {
                if let Some(n) = &f.name {
                    if env.vars.borrow().get(n.as_str()).is_none() {
                        let c = self.make_closure(f.clone(), env);
                        env.define(n, c);
                    }
                }
                Ok(V::Undef)
            }
            Stmt::Class(c) => {
                let v = self.make_class(c, env)?;
                if let Some(n) = &c.name {
                    env.define(n, v);
                }
                Ok(V::Undef)
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e, env)?,
                    None => self.undef(),
                };
                Err(Ctrl::Return(v))
            }
            Stmt::If(c, a, b) => {
                let cv = self.eval(c, env)?;
                if self.truthy(&cv) {
                    self.exec_body(a, env)
                } else if let Some(b) = b {
                    self.exec_body(b, env)
                } else {
                    Ok(V::Undef)
                }
            }
            Stmt::Block(b) => self.exec_block(b, env),
            Stmt::For { init, test, update, body } => {
                let scope = new_scope(Some(env.clone()), false);
                let per_iter = matches!(init.as_deref(), Some(Stmt::Var(VarKind::Let | VarKind::Const, _)));
                if let Some(i) = init {
                    self.exec(i, &scope)?;
                }
                let mut iter_scope = scope.clone();
                let mut last = V::Undef;
                loop {
                    if let Some(t) = test {
                        let tv = self.eval(t, &iter_scope)?;
                        if !self.truthy(&tv) {
                            break;
                        }
                    }
                    match self.exec_body(body, &iter_scope) {
                        Ok(v) => last = v,
                        Err(Ctrl::Break(l)) if Self::label_matches(&l, label) => break,
                        Err(Ctrl::Continue(l)) if Self::label_matches(&l, label) => {}
                        Err(e) => return Err(e),
                    }
                    if per_iter {
                        // fresh binding per iteration (closures capture the iteration's value)
                        let fresh = new_scope(Some(env.clone()), false);
                        for (k, v) in iter_scope.vars.borrow().iter() {
                            fresh.vars.borrow_mut().insert(k.clone(), v.clone());
                        }
                        iter_scope = fresh;
                    }
                    if let Some(u) = update {
                        self.eval(u, &iter_scope)?;
                    }
                }
                Ok(last)
            }
            Stmt::ForIn { decl, pat, obj, body, values, else_body } => {
                let o = self.eval(obj, env)?;
                if *values {
                    if let Some(iter) = self.lazy_iterator(&o)? {
                        let mut last = V::Undef;
                        let mut broke = false;
                        while let Some(item) = self.iter_next(&iter)? {
                            self.step()?;
                            let scope = if self.d == Dialect::Python { env.clone() } else { new_scope(Some(env.clone()), false) };
                            let mode = match decl {
                                Some(k) => Mode::Declare(*k),
                                None => Mode::Assign,
                            };
                            self.assign_pat(pat, item, &mode, &scope)?;
                            match self.exec_body(body, &scope) {
                                Ok(v) => last = v,
                                Err(Ctrl::Break(l)) if Self::label_matches(&l, label) => {
                                    broke = true;
                                    // close the iterator
                                    let _ = self.iter_resume(&iter, crate::gen::Resume::Return(V::Undef));
                                    break;
                                }
                                Err(Ctrl::Continue(l)) if Self::label_matches(&l, label) => {}
                                Err(e) => return Err(e),
                            }
                        }
                        if !broke {
                            if let Some(eb) = else_body {
                                self.exec_body(eb, env)?;
                            }
                        }
                        return Ok(last);
                    }
                }
                let items = if *values { self.iterate(&o, None)? } else if let Some(k) = crate::jsext::keys(self, &o)? { k } else { self.keys_of(&o) };
                let mut last = V::Undef;
                let mut broke = false;
                for item in items {
                    self.step()?;
                    let scope = if self.d == Dialect::Python { env.clone() } else { new_scope(Some(env.clone()), false) };
                    let mode = match decl {
                        Some(k) => Mode::Declare(*k),
                        None if self.d == Dialect::Python => Mode::Assign,
                        None => Mode::Assign,
                    };
                    self.assign_pat(pat, item, &mode, &scope)?;
                    match self.exec_body(body, &scope) {
                        Ok(v) => last = v,
                        Err(Ctrl::Break(l)) if Self::label_matches(&l, label) => {
                            broke = true;
                            break;
                        }
                        Err(Ctrl::Continue(l)) if Self::label_matches(&l, label) => {}
                        Err(e) => return Err(e),
                    }
                }
                if !broke {
                    if let Some(eb) = else_body {
                        self.exec_body(eb, env)?;
                    }
                }
                Ok(last)
            }
            Stmt::While(c, body, else_body) => {
                let mut last = V::Undef;
                let mut broke = false;
                loop {
                    self.step()?;
                    let cv = self.eval(c, env)?;
                    if !self.truthy(&cv) {
                        break;
                    }
                    match self.exec_body(body, env) {
                        Ok(v) => last = v,
                        Err(Ctrl::Break(l)) if Self::label_matches(&l, label) => {
                            broke = true;
                            break;
                        }
                        Err(Ctrl::Continue(l)) if Self::label_matches(&l, label) => {}
                        Err(e) => return Err(e),
                    }
                }
                if !broke {
                    if let Some(eb) = else_body {
                        self.exec_body(eb, env)?;
                    }
                }
                Ok(last)
            }
            Stmt::DoWhile(body, c) => {
                let mut last = V::Undef;
                loop {
                    self.step()?;
                    match self.exec_body(body, env) {
                        Ok(v) => last = v,
                        Err(Ctrl::Break(l)) if Self::label_matches(&l, label) => break,
                        Err(Ctrl::Continue(l)) if Self::label_matches(&l, label) => {}
                        Err(e) => return Err(e),
                    }
                    let cv = self.eval(c, env)?;
                    if !self.truthy(&cv) {
                        break;
                    }
                }
                Ok(last)
            }
            Stmt::Break(l) => Err(Ctrl::Break(l.clone())),
            Stmt::Continue(l) => Err(Ctrl::Continue(l.clone())),
            Stmt::Throw(Expr::Ident(n, _), pos) if n == "__reraise" => match self.exc_stack.last() {
                Some(e) => Err(Ctrl::Throw(e.clone())),
                None => self.throw("RuntimeError", "No active exception to reraise".into(), Some(*pos)),
            },
            Stmt::SwitchYield(e) => {
                let v = self.eval(e, env)?;
                Err(Ctrl::Value(v))
            }
            Stmt::Throw(e, pos) => {
                let v = self.eval(e, env)?;
                let v = match (&v, self.d) {
                    // Groovy / Python: throwing a string or a type
                    (V::Host(h), _) if matches!(**h, Host::Ns(_) | Host::NsFn(..)) => self.call(&v, V::Undef, vec![], *pos)?,
                    _ => v,
                };
                Err(Ctrl::Throw(v))
            }
            Stmt::Try { block, catches, finally } => {
                let r = self.exec_block(block, env);
                let r = match r {
                    Err(Ctrl::Throw(exc)) => {
                        let mut handled = None;
                        for c in catches {
                            if self.catch_matches(&c.types, &exc) {
                                let scope = if self.d == Dialect::Python { env.clone() } else { new_scope(Some(env.clone()), false) };
                                if let Some(p) = &c.param {
                                    let mode = if self.d == Dialect::Python { Mode::Assign } else { Mode::Declare(VarKind::Let) };
                                    self.assign_pat(p, exc.clone(), &mode, &scope)?;
                                }
                                self.exc_stack.push(exc.clone());
                                handled = Some(self.exec_block(&c.body, &scope));
                                self.exc_stack.pop();
                                break;
                            }
                        }
                        handled.unwrap_or(Err(Ctrl::Throw(exc)))
                    }
                    other => other,
                };
                if let Some(f) = finally {
                    self.exec_block(f, env)?;
                }
                r
            }
            Stmt::Switch(disc, cases) => {
                let dv = self.eval(disc, env)?;
                let scope = new_scope(Some(env.clone()), false);
                let mut matched = None;
                for (i, (test, _)) in cases.iter().enumerate() {
                    if let Some(t) = test {
                        let tv = self.eval(t, &scope)?;
                        if self.case_matches(&dv, &tv)? {
                            matched = Some(i);
                            break;
                        }
                    }
                }
                if matched.is_none() {
                    matched = cases.iter().position(|(t, _)| t.is_none());
                }
                let mut last = V::Undef;
                if let Some(start) = matched {
                    for (_, body) in &cases[start..] {
                        for st in body {
                            match self.exec(st, &scope) {
                                Ok(v) => last = v,
                                Err(Ctrl::Break(None)) => return Ok(last),
                                Err(e) => return Err(e),
                            }
                        }
                    }
                }
                Ok(last)
            }
            Stmt::Labeled(l, body) => match self.exec_labeled(body, env, Some(l)) {
                Err(Ctrl::Break(Some(x))) if x == *l => Ok(V::Undef),
                other => other,
            },
            Stmt::Empty => Ok(V::Undef),
            Stmt::Global(names) => {
                env.globals.borrow_mut().extend(names.iter().cloned());
                Ok(V::Undef)
            }
            Stmt::Assert(e, msg, pos) => {
                let v = self.eval(e, env)?;
                if !self.truthy(&v) {
                    let m = match msg {
                        Some(m) => {
                            let mv = self.eval(m, env)?;
                            self.display(&mv)
                        }
                        None => "assertion failed".to_string(),
                    };
                    let name = if self.d == Dialect::Python { "AssertionError" } else { "AssertionError" };
                    return self.throw(name, m, Some(*pos));
                }
                Ok(V::Undef)
            }
        }
    }

    fn exec_body(&mut self, s: &Stmt, env: &Env) -> R<V> {
        match s {
            Stmt::Block(b) => self.exec_block(b, env),
            other => self.exec(other, env),
        }
    }

    fn catch_matches(&self, types: &[String], exc: &V) -> bool {
        if types.is_empty() {
            return true;
        }
        let name = match exc {
            V::Obj(o) => o.borrow().props.get("name").map(|n| self.display(n)).unwrap_or_default(),
            _ => String::new(),
        };
        types.iter().any(|t| error_is_a(&name, t))
    }

    fn case_matches(&mut self, dv: &V, tv: &V) -> R<bool> {
        if self.d == Dialect::Js {
            return Ok(strict_eq(dv, tv));
        }
        if self.d == Dialect::Groovy {
            return Ok(match tv {
                V::Arr(a) => {
                    let items = a.borrow().clone();
                    items.iter().any(|x| self.equals(dv, x))
                }
                V::Regex(r) => r.re.is_full_match(&self.display(dv)),
                V::Host(h) => match &**h {
                    Host::Ns(n) => crate::builtins::is_type(dv, n),
                    Host::Type(n) => crate::builtins::is_type(dv, n),
                    _ => false,
                },
                V::Func(_) => {
                    let r = self.call(tv, V::Undef, vec![dv.clone()], 0)?;
                    self.truthy(&r)
                }
                _ => self.equals(dv, tv),
            });
        }
        Ok(self.equals(dv, tv))
    }

    // ------------------------------------------------------------------ variables
    pub fn undef(&self) -> V {
        if self.d == Dialect::Js {
            V::Undef
        } else {
            V::Null
        }
    }

    fn lookup_var(&self, name: &str, env: &Env) -> Option<V> {
        env.lookup(name)
    }

    fn lookup_ident(&mut self, name: &str, env: &Env, pos: u32) -> R<V> {
        if self.d.groovy_like() {
            if let Some(v) = self.lookup_with_fields(name, env) {
                return Ok(v);
            }
        } else if let Some(v) = env.lookup(name) {
            return Ok(v);
        }
        self.unresolved(name, env, pos)
    }

    fn unresolved(&mut self, name: &str, env: &Env, pos: u32) -> R<V> {
        match self.d {
            Dialect::Ognl | Dialect::Spel | Dialect::Mvel => {
                if let Some(this) = env.lookup("#this") {
                    if let V::Obj(o) = &this {
                        if let Some(v) = o.borrow().lookup(name) {
                            return Ok(v);
                        }
                    }
                    if !matches!(&this, V::Host(h) if **h == Host::Root) {
                        if let Some(v) = crate::host::root_member(self, name) {
                            return Ok(v);
                        }
                    }
                    if let Some(v) = crate::host::root_member(self, name) {
                        return Ok(v);
                    }
                }
                if let Some(v) = crate::builtins::type_ns(name) {
                    return Ok(v);
                }
                if self.d == Dialect::Mvel && name == "empty" {
                    return Ok(V::str(""));
                }
                let msg = match self.d {
                    Dialect::Spel => format!("EL1008E: Property or field '{}' cannot be found on object of type 'org.apache.camel.language.spel.RootObject'", name),
                    Dialect::Ognl => format!("No such property: {}", name),
                    _ => format!("unresolvable property or identifier: {}", name),
                };
                self.throw(if self.d == Dialect::Spel { "SpelEvaluationException" } else { "PropertyAccessException" }, msg, Some(pos))
            }
            Dialect::Js => self.throw("ReferenceError", format!("{} is not defined", name), Some(pos)),
            Dialect::Groovy => {
                if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    return Ok(crate::builtins::type_ns(name).unwrap_or_else(|| V::host(Host::Type(name.to_string()))));
                }
                self.throw("MissingPropertyException", format!("No such property: {} for class: Script1", name), Some(pos))
            }
            Dialect::Python => self.throw("NameError", format!("name '{}' is not defined", name), Some(pos)),
            Dialect::Jsonnet => self.throw("STATIC ERROR", format!("Unknown variable: {}", name), Some(pos)),
            Dialect::Java => {
                if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    return Ok(crate::builtins::type_ns(name).unwrap_or_else(|| V::host(Host::Type(name.to_string()))));
                }
                self.throw("CompilationException", format!("cannot find symbol: variable {}", name), Some(pos))
            }
        }
    }

    fn set_var(&mut self, name: &str, v: V, env: &Env) {
        if self.d == Dialect::Python {
            // local unless declared global
            let mut s = Some(env.clone());
            while let Some(sc) = s {
                if sc.globals.borrow().iter().any(|g| g == name) {
                    self.globals.define(name, v);
                    return;
                }
                if sc.func {
                    break;
                }
                s = sc.parent.clone();
            }
            env.define(name, v);
            return;
        }
        if self.d.groovy_like() {
            if !self.assign_with_fields(name, &v, env) {
                self.globals.define(name, v);
            }
            return;
        }
        if !env.assign(name, v.clone()) {
            self.globals.define(name, v);
        }
    }

    fn assign_pat(&mut self, pat: &Pat, v: V, mode: &Mode, env: &Env) -> R<()> {
        match pat {
            Pat::Id(n) => {
                match mode {
                    Mode::Declare(VarKind::Var) => {
                        let mut s = env.clone();
                        while !s.func {
                            match s.parent.clone() {
                                Some(p) => s = p,
                                None => break,
                            }
                        }
                        s.define(n, v);
                    }
                    Mode::Declare(_) => env.define(n, v),
                    Mode::Assign => self.set_var(n, v, env),
                }
                Ok(())
            }
            Pat::Target(e) => self.assign_target(e, v, env),
            Pat::Arr(items, rest) => {
                let vals = self.iterate(&v, None)?;
                for (i, it) in items.iter().enumerate() {
                    if let Some((p, def)) = it {
                        let mut x = vals.get(i).cloned().unwrap_or(V::Undef);
                        if matches!(x, V::Undef) {
                            x = match def {
                                Some(d) => self.eval(d, env)?,
                                None => self.undef(),
                            };
                        }
                        self.assign_pat(p, x, mode, env)?;
                    }
                }
                if let Some(r) = rest {
                    let tail: Vec<V> = vals.iter().skip(items.len()).cloned().collect();
                    self.assign_pat(r, V::arr(tail), mode, env)?;
                }
                Ok(())
            }
            Pat::Obj(props, rest) => {
                let mut used = Vec::new();
                for (k, p, def) in props {
                    let key = match k {
                        PropKey::Name(n) => n.clone(),
                        PropKey::Computed(e) => {
                            let kv = self.eval(e, env)?;
                            self.display(&kv)
                        }
                    };
                    let mut x = self.get_member(&v, &key, 0)?;
                    if matches!(x, V::Undef) {
                        if let Some(d) = def {
                            x = self.eval(d, env)?;
                        }
                    }
                    used.push(key);
                    self.assign_pat(p, x, mode, env)?;
                }
                if let Some(r) = rest {
                    let mut pm = PropMap::default();
                    if let V::Obj(o) = &v {
                        for (k, x) in o.borrow().props.iter() {
                            if !used.iter().any(|u| **u == **k) {
                                pm.set(k, x.clone());
                            }
                        }
                    }
                    self.assign_pat(r, V::obj(pm), mode, env)?;
                }
                Ok(())
            }
        }
    }

    fn assign_target(&mut self, e: &Expr, v: V, env: &Env) -> R<()> {
        match e {
            Expr::Ident(n, _) => {
                self.set_var(n, v, env);
                Ok(())
            }
            Expr::Member { obj, name, pos, .. } => {
                let o = self.eval(obj, env)?;
                self.set_member(&o, name, v, *pos)
            }
            Expr::Index { obj, index, pos, .. } => {
                let o = self.eval(obj, env)?;
                let k = self.eval(index, env)?;
                self.set_index(&o, &k, v, *pos)
            }
            _ => self.throw("SyntaxError", "invalid assignment target".into(), None),
        }
    }

    // ------------------------------------------------------------------ functions
    pub fn make_closure(&self, def: Arc<FuncDef>, env: &Env) -> V {
        self.track_env(env);
        V::Func(new_closure(Closure { def, env: env.clone(), props: RefCell::new(PropMap::default()), parent: None, home: RefCell::new(None), flavor: Flavor::Plain }))
    }

    fn track_env(&self, env: &Env) {
        let mut envs = self.closure_envs.borrow_mut();
        if !envs.last().is_some_and(|w| std::ptr::eq(w.as_ptr(), Rc::as_ptr(env))) {
            envs.push(Rc::downgrade(env));
        }
    }

    /// Break reference cycles between scopes and closures (call after evaluation).
    pub fn teardown(&mut self) {
        let envs = std::mem::take(&mut *self.closure_envs.borrow_mut());
        for w in envs {
            let mut cur = w.upgrade();
            while let Some(s) = cur {
                let vars = std::mem::take(&mut *s.vars.borrow_mut());
                drop(vars);
                cur = s.parent.clone();
            }
        }
        let g = std::mem::take(&mut *self.globals.vars.borrow_mut());
        drop(g);
        self.ystack.clear();
        self.queues = Default::default();
        self.classes.clear();
        self.symbols.clear();
        self.imports.clear();
        self.meta_methods.clear();
        self.beans.clear();
        self.exc_stack.clear();
    }

    /// The `prototype` object of a function (created on demand).
    pub fn func_prototype(&self, c: &Rc<Closure>) -> Option<V> {
        if let Some(p) = c.props.borrow().get("prototype") {
            return Some(p.clone());
        }
        let proto = V::new_obj();
        c.props.borrow_mut().set("prototype", proto.clone());
        Some(proto)
    }

    /// Call a function value.
    pub fn call(&mut self, f: &V, this: V, args: Vec<V>, pos: u32) -> R<V> {
        match f {
            V::Func(c) if c.flavor == Flavor::Class => {
                if self.d == Dialect::Js {
                    let n = c.def.name.clone().unwrap_or_default();
                    return self.type_error(format!("Class constructor {} cannot be invoked without 'new'", n), Some(pos));
                }
                self.construct_class(c, args, pos)
            }
            V::Func(c) => self.call_closure(c, this, args, pos),
            V::Obj(_) if crate::jsext::is_proxy(f) => crate::jsext::proxy_call(self, f, this, args, pos),
            V::Obj(_) if self.user_method(f, if self.d == Dialect::Python { "__call__" } else { "call" }).is_some() => {
                let name = if self.d == Dialect::Python { "__call__" } else { "call" };
                let m = self.user_method(f, name).unwrap();
                self.call_user_method(f, &m, args, pos)
            }
            V::Method(m) => {
                let (recv, name) = (&m.0, &m.1);
                if let Some(r) = name.strip_prefix("%mref:") {
                    return crate::java::method_ref_call(self, recv, r, args, pos);
                }
                self.call_method(recv, name, args, pos)
            }
            V::Host(h) => match &**h {
                Host::Ns(n) => crate::builtins::call_ns(self, n, args, pos),
                Host::NsFn(ns, name) => crate::builtins::call_ns_fn(self, ns, name, args, pos),
                Host::Type(n) => crate::builtins::construct_type(self, n, args, pos),
                _ => self.type_error(format!("{} is not a function", self.display(f)), Some(pos)),
            },
            V::Regex(_) if self.d == Dialect::Groovy => Ok(f.clone()),
            _ => {
                let what = match self.d {
                    Dialect::Python => format!("'{}' object is not callable", crate::builtins::py_type_name(f)),
                    _ => format!("{} is not a function", self.display_short(f)),
                };
                self.type_error(what, Some(pos))
            }
        }
    }

    fn call_closure(&mut self, c: &Rc<Closure>, this: V, args: Vec<V>, pos: u32) -> R<V> {
        if crate::stack::low_on_stack() && self.depth < self.max_depth {
            // continue a deep recursion on a fresh stack segment
            let mut out = None;
            let done = crate::stack::on_new_segment(|| {
                out = Some(self.call_closure_inner(c, this.clone(), args.clone(), pos));
            });
            if done.is_some() {
                if let Some(r) = out {
                    return r;
                }
            }
            return self.stack_overflow(pos);
        }
        self.call_closure_inner(c, this, args, pos)
    }

    fn stack_overflow<X>(&self, pos: u32) -> R<X> {
        let (name, msg) = match self.d {
            Dialect::Js => ("RangeError", "Maximum call stack size exceeded"),
            Dialect::Python => ("RecursionError", "maximum recursion depth exceeded"),
            Dialect::Groovy | Dialect::Java => ("StackOverflowError", "maximum call depth exceeded"),
            _ => ("Error", "maximum call depth exceeded"),
        };
        self.throw(name, msg.into(), Some(pos))
    }

    fn call_closure_inner(&mut self, c: &Rc<Closure>, this: V, mut args: Vec<V>, pos: u32) -> R<V> {
        if self.depth >= self.max_depth {
            let (name, msg) = match self.d {
                Dialect::Js => ("RangeError", "Maximum call stack size exceeded"),
                Dialect::Python => ("RecursionError", "maximum recursion depth exceeded"),
                Dialect::Groovy => ("StackOverflowError", "maximum call depth exceeded"),
                _ => ("Error", "maximum call depth exceeded"),
            };
            return self.throw(name, msg.into(), Some(pos));
        }
        self.step()?;
        let def = c.def.clone();
        let scope = new_scope(Some(c.env.clone()), true);
        if !def.arrow {
            scope.define("this", this.clone());
        }
        if let Some(home) = c.home.borrow().clone() {
            scope.define("%home", V::Obj(home));
        }
        if let Some(p) = &c.parent {
            scope.define("%parent", p.clone());
        }
        if c.flavor == Flavor::PyMethod {
            if let Some(s) = args.first() {
                scope.define("%self", s.clone());
            }
        }
        if self.d == Dialect::Groovy && def.implicit_return && !matches!(this, V::Undef) && c.home.borrow().is_none() {
            // Groovy closures: `delegate` (metaclass methods, `with`)
            scope.define("delegate", this.clone());
        }
        // kwargs (Python)
        let mut kwargs: Option<ObjRef> = None;
        if let Some(V::Obj(o)) = args.last() {
            if o.borrow().kind == ObjKind::Kwargs {
                kwargs = Some(o.clone());
            }
        }
        if kwargs.is_some() {
            args.pop();
        }
        // Groovy: a single list argument is spread over several closure parameters
        if self.d == Dialect::Groovy && def.params.len() > 1 && args.len() == 1 {
            if let V::Arr(a) = &args[0] {
                let items = a.borrow().clone();
                args = items;
            }
        }
        if def.implicit_it && def.params.is_empty() {
            scope.define("it", args.first().cloned().unwrap_or(V::Null));
        }
        if self.d == Dialect::Js && !def.arrow {
            scope.define("arguments", V::arr(args.clone()));
        }
        for (i, p) in def.params.iter().enumerate() {
            let mut v = args.get(i).cloned().unwrap_or(V::Undef);
            if let (Some(kw), Pat::Id(n)) = (&kwargs, &p.pat) {
                if let Some(x) = kw.borrow().props.get(n) {
                    v = x.clone();
                }
            }
            if matches!(v, V::Undef) {
                v = match &p.default {
                    Some(d) => self.eval(d, &scope)?,
                    None => self.undef(),
                };
            }
            self.assign_pat(&p.pat, v, &Mode::Declare(VarKind::Let), &scope)?;
        }
        if let Some(r) = &def.rest {
            let tail: Vec<V> = args.iter().skip(def.params.len()).cloned().collect();
            self.assign_pat(r, V::arr(tail), &Mode::Declare(VarKind::Let), &scope)?;
        }
        if def.generator || def.is_async {
            // `async function*` runs as a generator whose elements `for await` awaits
            let g = self.make_generator(c, scope, def.is_async);
            if def.is_async && !def.generator {
                return self.start_async(g);
            }
            return Ok(self.generator_value(g));
        }
        self.depth += 1;
        let r = match &def.body {
            FuncBody::Expr(e) => self.eval(e, &scope),
            FuncBody::Block(b) => match self.exec_block(b, &scope) {
                Ok(v) => Ok(if def.implicit_return { v } else { self.undef() }),
                Err(Ctrl::Return(v)) => Ok(v),
                Err(e) => Err(e),
            },
        };
        self.depth -= 1;
        match r {
            Err(Ctrl::Break(_)) | Err(Ctrl::Continue(_)) => Ok(self.undef()),
            other => other,
        }
    }

    /// `new f(args)`.
    pub(crate) fn construct_pub(&mut self, f: &V, args: Vec<V>, pos: u32) -> R<V> {
        self.construct(f, args, pos)
    }

    /// Construct with `new`.
    fn construct(&mut self, f: &V, args: Vec<V>, pos: u32) -> R<V> {
        match f {
            V::Obj(_) if crate::jsext::is_proxy(f) => crate::jsext::proxy_construct(self, f, args, pos),
            V::Func(c) => self.construct_class(c, args, pos),
            V::Host(h) => match &**h {
                Host::Ns(n) => crate::builtins::construct_ns(self, n, args, pos),
                Host::Type(n) => crate::builtins::construct_type(self, n, args, pos),
                _ => self.type_error(format!("{} is not a constructor", self.display(f)), Some(pos)),
            },
            _ => self.type_error(format!("{} is not a constructor", self.display_short(f)), Some(pos)),
        }
    }

    /// Call a method on a value: own/prototype functions first, then built-in methods.
    pub fn call_method(&mut self, recv: &V, name: &str, args: Vec<V>, pos: u32) -> R<V> {
        if crate::jsext::is_proxy(recv) && !name.starts_with('%') {
            let f = crate::jsext::proxy_get(self, recv, &V::str(name), pos)?;
            if !f.is_callable() {
                return self.type_error(format!("{} is not a function", name), Some(pos));
            }
            return self.call(&f, recv.clone(), args, pos);
        }
        match recv {
            V::Obj(o) => {
                let (f, own) = {
                    let b = o.borrow();
                    match b.props.get(name) {
                        Some(f) => (Some(f.clone()), true),
                        None => (b.lookup(name), false),
                    }
                };
                if let Some(f) = f {
                    if let V::Obj(acc) = &f {
                        if acc.borrow().kind == ObjKind::Accessor {
                            let g = self.call_getter(acc, recv, pos)?;
                            return self.call(&g, recv.clone(), args, pos);
                        }
                    }
                    if f.is_callable() {
                        if !own {
                            return self.call_user_method(recv, &f, args, pos);
                        }
                        return self.call(&f, recv.clone(), args, pos);
                    }
                }
                if self.d.groovy_like() {
                    if let Some(v) = self.groovy_method_fallback(recv, name, &args, pos)? {
                        return Ok(v);
                    }
                }
                if matches!(o.borrow().kind, ObjKind::Jsonnet(_)) {
                    // hidden / late-bound DataSonnet method fields
                    let o = o.clone();
                    if let Some(f) = self.jfield(&o, name)? {
                        return self.call(&f, V::Undef, args, pos);
                    }
                }
            }
            V::Func(c) if c.flavor == Flavor::Class => {
                let f = c.props.borrow().get(name).cloned();
                let f = match f {
                    Some(f) => Some(f),
                    None => match self.func_prototype(c) {
                        Some(V::Obj(p)) => p.borrow().lookup(name),
                        _ => None,
                    },
                };
                if let Some(f) = f {
                    if let V::Func(m) = &f {
                        if m.flavor == Flavor::PyClassMethod {
                            let mut a = vec![recv.clone()];
                            a.extend(args);
                            return self.call(&f, V::Undef, a, pos);
                        }
                    }
                    if f.is_callable() {
                        return self.call(&f, recv.clone(), args, pos);
                    }
                }
                if name == "new" {
                    return self.construct_class(c, args, pos);
                }
            }
            V::Func(c) => {
                match name {
                    "call" if self.d == Dialect::Js => {
                        let mut a = args;
                        let this = if a.is_empty() { V::Undef } else { a.remove(0) };
                        return self.call_closure(c, this, a, pos);
                    }
                    "call" | "doCall" => return self.call_closure(c, V::Undef, args, pos),
                    "apply" if self.d != Dialect::Java => {
                        let this = args.first().cloned().unwrap_or(V::Undef);
                        let a = match args.get(1) {
                            Some(v) => self.iterate(v, None)?,
                            None => vec![],
                        };
                        return self.call_closure(c, this, a, pos);
                    }
                    "bind" => {
                        // a bound function: capture `this` and the leading arguments
                        let this = args.first().cloned().unwrap_or(V::Undef);
                        let bound: Vec<V> = args.iter().skip(1).cloned().collect();
                        let mut p = PropMap::default();
                        p.set("fn", recv.clone());
                        p.set("this", this);
                        p.set("args", V::arr(bound));
                        return Ok(V::Method(Rc::new((V::obj(p), "%bound".to_string()))));
                    }
                    _ => {}
                }
                let f = c.props.borrow().get(name).cloned();
                if let Some(f) = f {
                    return self.call(&f, recv.clone(), args, pos);
                }
            }
            V::Host(h) => {
                let h = h.clone();
                return crate::host::call_method(self, &h, recv, name, args, pos);
            }
            _ => {}
        }
        if self.d.groovy_like() && !self.meta_methods.is_empty() {
            if let Some(m) = self.meta_method(recv, name) {
                return self.call(&m, recv.clone(), args, pos);
            }
        }
        crate::builtins::call_method(self, recv, name, args, pos)
    }

    // ------------------------------------------------------------------ members
    /// Property read.
    pub fn get_member(&mut self, o: &V, name: &str, pos: u32) -> R<V> {
        match o {
            V::Undef | V::Null => {
                if self.d == Dialect::Groovy && name == "class" {
                    return Ok(V::Null);
                }
                self.null_error("read", name, o, pos)
            }
            V::Obj(ob) => {
                if crate::jsext::is_ext(ob) {
                    if let Some(v) = crate::jsext::get_member(self, o, name, pos)? {
                        return Ok(v);
                    }
                }
                if let (Some(field), Dialect::Groovy) = (name.strip_prefix('@'), self.d) {
                    return Ok(ob.borrow().props.get(field).cloned().unwrap_or(V::Null));
                }
                if self.d.groovy_like() && ob.borrow().proto.is_some() {
                    if let Some(v) = self.groovy_get_property(o, ob, name, pos)? {
                        return Ok(v);
                    }
                }
                let (r, own) = {
                    let b = ob.borrow();
                    match b.props.get(name) {
                        Some(v) => (Some(v.clone()), true),
                        None => (b.lookup(name), false),
                    }
                };
                match r {
                    Some(V::Obj(acc)) if acc.borrow().kind == ObjKind::Accessor => self.call_getter(&acc, o, pos),
                    Some(V::Func(f)) if !own && matches!(f.flavor, Flavor::PyMethod | Flavor::PyClassMethod) => Ok(V::Method(Rc::new((o.clone(), name.to_string())))),
                    Some(v) => Ok(v),
                    None => {
                        if matches!(ob.borrow().kind, ObjKind::Jsonnet(_)) {
                            if let Some(v) = self.jfield(ob, name)? {
                                return Ok(v);
                            }
                        }
                        let v = crate::builtins::object_property(self, o, name, pos)?;
                        if self.d == Dialect::Python && matches!(v, V::Undef | V::Null) && !name.starts_with("__") {
                            if let Some(c) = crate::classes::class_of(o) {
                                let cname = c.def.name.clone().unwrap_or_else(|| "object".into());
                                return self.throw("AttributeError", format!("'{}' object has no attribute '{}'", cname, name), Some(pos));
                            }
                        }
                        Ok(v)
                    }
                }
            }
            V::Func(c) => {
                if name == "prototype" {
                    return Ok(self.func_prototype(c).unwrap_or(V::Undef));
                }
                if let Some(v) = c.props.borrow().get(name) {
                    if let V::Obj(acc) = v {
                        if acc.borrow().kind == ObjKind::Accessor {
                            let acc = acc.clone();
                            return self.call_getter(&acc, o, pos);
                        }
                    }
                    return Ok(v.clone());
                }
                if c.flavor == Flavor::Class {
                    if name == "metaClass" && self.d == Dialect::Groovy {
                        return Ok(V::host(Host::Meta(c.def.name.clone().unwrap_or_default())));
                    }
                    if name == "__name__" || name == "simpleName" {
                        return Ok(V::str(c.def.name.as_deref().unwrap_or("")));
                    }
                    // Python class attributes / methods, Groovy statics of parents
                    let proto = self.func_prototype(c);
                    if let Some(V::Obj(p)) = proto {
                        if self.d == Dialect::Python {
                            if let Some(v) = p.borrow().lookup(name) {
                                return Ok(v);
                            }
                        }
                    }
                    if let Some(V::Func(pc)) = &c.parent {
                        let pv = V::Func(pc.clone());
                        return self.get_member(&pv, name, pos);
                    }
                }
                match name {
                    "name" => Ok(V::str(c.def.name.as_deref().unwrap_or(""))),
                    "length" => Ok(V::Int(c.def.params.len() as i64)),
                    _ => Ok(self.undef()),
                }
            }
            V::Host(h) => {
                let h = h.clone();
                crate::host::get_member(self, &h, o, name, pos)
            }
            _ => crate::builtins::value_property(self, o, name, pos),
        }
    }

    /// Property write.
    pub fn set_member(&mut self, o: &V, name: &str, v: V, pos: u32) -> R<()> {
        if let (Some(field), V::Obj(ob), Dialect::Groovy) = (name.strip_prefix('@'), o, self.d) {
            ob.borrow_mut().props.set(field, v);
            return Ok(());
        }
        if let V::Obj(ob) = o {
            if crate::jsext::is_ext(ob) && crate::jsext::set_index(self, o, &V::str(name), v.clone(), pos)? {
                return Ok(());
            }
        }
        match o {
            V::Obj(ob) => {
                let has_proto = ob.borrow().proto.is_some();
                if has_proto || matches!(ob.borrow().props.get(name), Some(V::Obj(_))) {
                    if let Some(acc) = Self::find_accessor(ob, name) {
                        return self.call_setter(&acc, o, name, v, pos);
                    }
                    if has_proto && self.d.groovy_like() && crate::classes::class_of(o).is_some() {
                        if let Some(f) = self.user_method(o, &format!("set{}", crate::classes::cap(name))) {
                            return self.call(&f, o.clone(), vec![v], pos).map(|_| ());
                        }
                    }
                }
                ob.borrow_mut().props.set(name, v);
                Ok(())
            }
            V::Func(c) => {
                if let Some(V::Obj(acc)) = c.props.borrow().get(name).cloned() {
                    if acc.borrow().kind == ObjKind::Accessor {
                        return self.call_setter(&acc, o, name, v, pos);
                    }
                }
                if c.flavor == Flavor::Class && self.d == Dialect::Python {
                    if let Some(V::Obj(p)) = self.func_prototype(c) {
                        p.borrow_mut().props.set(name, v);
                        return Ok(());
                    }
                }
                if c.flavor == Flavor::Class && self.d == Dialect::Groovy && !c.props.borrow().contains(name) {
                    // static of a parent class
                    let mut cur = c.parent.clone();
                    while let Some(V::Func(p)) = cur {
                        if p.props.borrow().contains(name) {
                            p.props.borrow_mut().set(name, v);
                            return Ok(());
                        }
                        cur = p.parent.clone();
                    }
                }
                c.props.borrow_mut().set(name, v);
                Ok(())
            }
            V::Host(h) if matches!(&**h, Host::Meta(_)) => {
                if let Host::Meta(t) = &**h {
                    self.meta_methods.entry(t.clone()).or_default().insert(name.to_string(), v);
                }
                Ok(())
            }
            V::Arr(a) => {
                if let Ok(i) = name.parse::<i64>() {
                    return self.set_index(o, &V::Int(i), v, pos);
                }
                if name == "length" {
                    let n = v.as_f64().unwrap_or(0.0) as usize;
                    a.borrow_mut().resize(n, V::Undef);
                    return Ok(());
                }
                if self.d == Dialect::Groovy {
                    // GPath: set the property on every element
                    let items = a.borrow().clone();
                    for it in items {
                        self.set_member(&it, name, v.clone(), pos)?;
                    }
                    return Ok(());
                }
                self.type_error(format!("cannot set property '{}' of an array", name), Some(pos))
            }
            V::Host(h) => {
                let h = h.clone();
                crate::host::set_member(self, &h, name, v, pos)
            }
            V::Undef | V::Null => self.null_error("set", name, o, pos),
            _ => {
                if self.d == Dialect::Js {
                    Ok(())
                } else {
                    self.type_error(format!("cannot set property '{}' on {}", name, self.display_short(o)), Some(pos))
                }
            }
        }
    }

    /// Index read (`a[i]`, `m['k']`).
    pub fn get_index(&mut self, o: &V, k: &V, pos: u32) -> R<V> {
        if let V::Obj(ob) = o {
            if crate::jsext::is_ext(ob) {
                if let Some(v) = crate::jsext::get_index(self, o, k, pos)? {
                    return Ok(v);
                }
            }
        }
        if self.d == Dialect::Python {
            if let V::Obj(ob) = o {
                if ob.borrow().proto.is_some() {
                    if let Some(v) = self.py_special(o, "__getitem__", vec![k.clone()], pos)? {
                        return Ok(v);
                    }
                }
            }
        }
        match (o, k) {
            (V::Arr(a), _) if k.is_number() => {
                let i = k.as_f64().unwrap_or(0.0);
                if i.fract() != 0.0 {
                    return Ok(self.undef());
                }
                let len = a.borrow().len() as i64;
                let mut i = i as i64;
                if i < 0 && matches!(self.d, Dialect::Groovy | Dialect::Python | Dialect::Mvel) {
                    i += len;
                }
                if i < 0 || i >= len {
                    return match self.d {
                        Dialect::Python => self.throw("IndexError", "list index out of range".into(), Some(pos)),
                        Dialect::Jsonnet => self.throw("RUNTIME ERROR", format!("array bounds error: {} not within [0, {})", i, len), Some(pos)),
                        Dialect::Spel => self.throw("SpelEvaluationException", format!("EL1025E: The collection has '{}' elements, index '{}' is invalid", len, i), Some(pos)),
                        _ => Ok(self.undef()),
                    };
                }
                Ok(a.borrow()[i as usize].clone())
            }
            (V::Arr(a), V::Arr(idx)) if self.d == Dialect::Groovy => {
                let idx = idx.borrow().clone();
                let mut out = Vec::new();
                for i in idx {
                    out.push(self.get_index(&V::Arr(a.clone()), &i, pos)?);
                }
                Ok(V::arr(out))
            }
            (V::Str(s), _) if k.is_number() => {
                let chars: Vec<char> = s.chars().collect();
                let mut i = k.as_f64().unwrap_or(0.0) as i64;
                if i < 0 && self.d != Dialect::Js {
                    i += chars.len() as i64;
                }
                if i < 0 || i as usize >= chars.len() {
                    return match self.d {
                        Dialect::Python => self.throw("IndexError", "string index out of range".into(), Some(pos)),
                        Dialect::Groovy => self.throw("StringIndexOutOfBoundsException", format!("String index out of range: {}", i), Some(pos)),
                        _ => Ok(self.undef()),
                    };
                }
                Ok(V::string(chars[i as usize].to_string()))
            }
            (V::Str(s), V::Arr(idx)) if self.d == Dialect::Groovy => {
                let chars: Vec<char> = s.chars().collect();
                let mut out = String::new();
                for i in idx.borrow().iter() {
                    let mut i = i.as_f64().unwrap_or(0.0) as i64;
                    if i < 0 {
                        i += chars.len() as i64;
                    }
                    if let Some(c) = chars.get(i as usize) {
                        out.push(*c);
                    }
                }
                Ok(V::string(out))
            }
            (V::Obj(ob), _) => {
                let key = self.key_string(k);
                let mut r = ob.borrow().lookup(&key);
                if r.is_none() && matches!(ob.borrow().kind, ObjKind::Jsonnet(_)) {
                    let ob = ob.clone();
                    r = self.jfield(&ob, &key)?;
                }
                match r {
                    Some(v) => Ok(v),
                    None => match self.d {
                        Dialect::Python => self.throw("KeyError", crate::builtins::py_repr(self, k), Some(pos)),
                        Dialect::Jsonnet => self.throw("RUNTIME ERROR", format!("field does not exist: {}", key), Some(pos)),
                        Dialect::Js => crate::builtins::object_property(self, o, &key, pos),
                        _ => Ok(V::Null),
                    },
                }
            }
            (V::Undef | V::Null, _) => {
                let key = self.key_string(k);
                self.null_error("read", &key, o, pos)
            }
            _ => {
                let key = self.key_string(k);
                self.get_member(o, &key, pos)
            }
        }
    }

    /// Index write.
    pub fn set_index(&mut self, o: &V, k: &V, v: V, pos: u32) -> R<()> {
        if let V::Obj(ob) = o {
            if crate::jsext::is_ext(ob) && crate::jsext::set_index(self, o, k, v.clone(), pos)? {
                return Ok(());
            }
        }
        if self.d == Dialect::Python {
            if let V::Obj(ob) = o {
                if ob.borrow().proto.is_some() && self.user_method(o, "__setitem__").is_some() {
                    self.py_special(o, "__setitem__", vec![k.clone(), v], pos)?;
                    return Ok(());
                }
            }
        }
        match (o, k) {
            (V::Arr(a), _) if k.is_number() => {
                let mut i = k.as_f64().unwrap_or(0.0) as i64;
                let mut arr = a.borrow_mut();
                if i < 0 && self.d != Dialect::Js {
                    i += arr.len() as i64;
                }
                if i < 0 {
                    drop(arr);
                    return self.throw(if self.d == Dialect::Python { "IndexError" } else { "IndexOutOfBoundsException" }, "list assignment index out of range".into(), Some(pos));
                }
                let i = i as usize;
                if i >= arr.len() {
                    if self.d == Dialect::Python {
                        drop(arr);
                        return self.throw("IndexError", "list assignment index out of range".into(), Some(pos));
                    }
                    let fill = if self.d == Dialect::Js { V::Undef } else { V::Null };
                    arr.resize(i + 1, fill);
                }
                arr[i] = v;
                Ok(())
            }
            _ => {
                let key = self.key_string(k);
                self.set_member(o, &key, v, pos)
            }
        }
    }

    /// Convert a key value to a property name.
    pub fn key_string(&self, k: &V) -> String {
        match k {
            V::Str(s) => s.to_string(),
            V::Int(i) => i.to_string(),
            V::Num(f) => js_num_str(*f),
            V::Bool(b) => if self.d == Dialect::Python { if *b { "True" } else { "False" } } else if *b { "true" } else { "false" }.to_string(),
            V::Null => if self.d == Dialect::Python { "None" } else { "null" }.to_string(),
            V::Obj(o) if matches!(o.borrow().kind, ObjKind::Symbol(..)) => match &o.borrow().kind {
                ObjKind::Symbol(id, d) => crate::collections::symbol_key(*id, d),
                _ => unreachable!(),
            },
            other => self.display(other),
        }
    }

    // ------------------------------------------------------------------ iteration
    /// Values produced by iterating `v` (for-of, Groovy for-in, Python for, spread).
    pub fn iterate(&mut self, v: &V, pos: Option<u32>) -> R<Vec<V>> {
        if let V::Obj(ob) = v {
            if crate::jsext::is_ext(ob) {
                if let Some(items) = crate::jsext::iterate(self, v, pos)? {
                    return Ok(items);
                }
            }
        }
        Ok(match v {
            V::Arr(a) => a.borrow().clone(),
            V::Str(s) => s.chars().map(|c| V::string(c.to_string())).collect(),
            V::Bytes(b) => b.iter().map(|x| V::Int(*x as i64)).collect(),
            V::Obj(ob) if !matches!(ob.borrow().kind, ObjKind::Plain | ObjKind::Set | ObjKind::Kwargs | ObjKind::Jsonnet(_)) || (ob.borrow().proto.is_some() && self.d != Dialect::Groovy) || (self.d == Dialect::Js && ob.borrow().kind == ObjKind::Plain) => {
                let kind = ob.borrow().kind.clone();
                if let ObjKind::Coll(c) = &kind {
                    let items = crate::collections::iterate_coll(&c.0.borrow());
                    if let Some(items) = items {
                        if matches!(&*c.0.borrow(), crate::collections::Coll::Iter(..)) {
                            if let crate::collections::Coll::Iter(_, i) = &mut *c.0.borrow_mut() {
                                *i = usize::MAX / 2;
                            }
                        }
                        return Ok(items);
                    }
                }
                if let Some(it) = self.lazy_iterator(v)? {
                    let mut out = Vec::new();
                    while let Some(x) = self.iter_next(&it)? {
                        self.step()?;
                        out.push(x);
                    }
                    return Ok(out);
                }
                if self.d == Dialect::Js {
                    return self.type_error(format!("{} is not iterable", self.display_short(v)), pos);
                }
                let o = ob.borrow();
                match (&o.kind, self.d) {
                    (_, Dialect::Python) | (_, Dialect::Jsonnet) => o.props.keys().map(|k| V::Str(k.clone())).collect(),
                    _ => o.props.iter().map(|(k, x)| crate::builtins::entry(k, x.clone())).collect(),
                }
            }
            V::Obj(o) => {
                let o = o.borrow();
                match (&o.kind, self.d) {
                    (ObjKind::Set, _) => o.props.values().cloned().collect(),
                    (_, Dialect::Python) | (_, Dialect::Jsonnet) => o.props.keys().map(|k| V::Str(k.clone())).collect(),
                    (_, Dialect::Js) => {
                        drop(o);
                        return self.type_error(format!("{} is not iterable", self.display_short(v)), pos);
                    }
                    _ => o.props.iter().map(|(k, x)| crate::builtins::entry(k, x.clone())).collect(),
                }
            }
            V::Host(h) => {
                let snap = crate::host::snapshot(self, h);
                match snap {
                    Some(s) => return self.iterate(&s, pos),
                    None => vec![v.clone()],
                }
            }
            V::Null | V::Undef if self.d == Dialect::Groovy => vec![],
            V::Null | V::Undef => {
                let msg = match self.d {
                    Dialect::Python => "'NoneType' object is not iterable".to_string(),
                    _ => format!("{} is not iterable", self.display(v)),
                };
                return self.type_error(msg, pos);
            }
            other if self.d == Dialect::Groovy => vec![other.clone()],
            other => {
                let msg = match self.d {
                    Dialect::Python => format!("'{}' object is not iterable", crate::builtins::py_type_name(other)),
                    _ => format!("{} is not iterable", self.display(other)),
                };
                return self.type_error(msg, pos);
            }
        })
    }

    /// Keys for JS `for (k in obj)`.
    fn keys_of(&self, v: &V) -> Vec<V> {
        match v {
            V::Arr(a) => (0..a.borrow().len()).map(|i| V::string(i.to_string())).collect(),
            V::Str(s) => (0..s.chars().count()).map(|i| V::string(i.to_string())).collect(),
            V::Obj(o) => {
                let mut keys: Vec<V> = Vec::new();
                let o = o.borrow();
                for k in o.props.keys() {
                    if !matches!(o.props.get(k), Some(V::Func(_))) || o.proto.is_none() {
                        keys.push(V::Str(k.clone()));
                    }
                }
                let mut p = o.proto.clone();
                while let Some(pr) = p {
                    let b = pr.borrow();
                    for (k, x) in b.props.iter() {
                        if !matches!(x, V::Func(_)) && &**k != "constructor" {
                            keys.push(V::Str(k.clone()));
                        }
                    }
                    p = b.proto.clone();
                }
                keys
            }
            _ => vec![],
        }
    }

    // ------------------------------------------------------------------ expressions
    fn eval_named(&mut self, e: &Expr, env: &Env, pat: &Pat) -> R<V> {
        let v = self.eval(e, env)?;
        if let (V::Func(c), Pat::Id(n)) = (&v, pat) {
            if c.def.name.is_none() && !c.props.borrow().contains("name") {
                c.props.borrow_mut().set("name", V::str(n));
            }
        }
        Ok(v)
    }

    fn eval_args(&mut self, args: &[Elem], env: &Env) -> R<Vec<V>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            match a {
                Elem::Item(e) => out.push(self.eval(e, env)?),
                Elem::Spread(e) => {
                    let v = self.eval(e, env)?;
                    out.extend(self.iterate(&v, None)?);
                }
                Elem::Hole => out.push(V::Undef),
            }
        }
        Ok(out)
    }

    /// Evaluate a member/call chain; `None` when an optional link short-circuited.
    fn eval_chain(&mut self, e: &Expr, env: &Env) -> R<Option<V>> {
        match e {
            Expr::Member { obj, name, optional, pos } => {
                let o = match self.eval_chain(obj, env)? {
                    Some(o) => o,
                    None => return Ok(None),
                };
                if *optional && o.is_nullish() {
                    return Ok(None);
                }
                Ok(Some(self.get_member(&o, name, *pos)?))
            }
            Expr::Index { obj, index, optional, pos } => {
                let o = match self.eval_chain(obj, env)? {
                    Some(o) => o,
                    None => return Ok(None),
                };
                if *optional && o.is_nullish() {
                    return Ok(None);
                }
                if let Expr::Range { from, to, exclusive } = &**index {
                    let f = self.eval(from, env)?;
                    let t = self.eval(to, env)?;
                    return Ok(Some(crate::builtins::range_slice(self, &o, &f, &t, *exclusive, *pos)?));
                }
                let k = self.eval(index, env)?;
                Ok(Some(self.get_index(&o, &k, *pos)?))
            }
            Expr::Call { callee, args, optional, pos } => self.eval_call(callee, args, *optional, *pos, env),
            other => Ok(Some(self.eval(other, env)?)),
        }
    }

    fn eval_call(&mut self, callee: &Expr, args: &[Elem], optional: bool, pos: u32, env: &Env) -> R<Option<V>> {
        if let Expr::Member { obj, name, pos: mpos, .. } = callee {
            if let Expr::Super(_) = &**obj {
                let (f, this) = self.super_method(env, name, *mpos)?;
                let a = self.eval_args(args, env)?;
                if let V::Method(m) = &f {
                    if let Some(n) = m.1.strip_prefix("%super:") {
                        let n = n.to_string();
                        return Ok(Some(self.super_builtin(&this, &n, &a, *mpos)?));
                    }
                }
                return Ok(Some(self.call_user_method(&this, &f, a, *mpos)?));
            }
        }
        match callee {
            Expr::Member { obj, name, optional: mopt, pos: mpos } => {
                let o = match self.eval_chain(obj, env)? {
                    Some(o) => o,
                    None => return Ok(None),
                };
                if (*mopt || optional) && o.is_nullish() {
                    return Ok(None);
                }
                if optional && matches!(o, V::Obj(_)) {
                    let f = self.get_member(&o, name, *mpos)?;
                    if f.is_nullish() {
                        return Ok(None);
                    }
                }
                let a = self.eval_args(args, env)?;
                if o.is_nullish() {
                    return match self.d {
                        Dialect::Groovy => self.throw("NullPointerException", format!("Cannot invoke method {}() on null object", name), Some(*mpos)),
                        Dialect::Spel => self.throw("SpelEvaluationException", format!("EL1011E: Method call: Attempted to call method {}() on null context object", name), Some(*mpos)),
                        Dialect::Python => self.throw("AttributeError", format!("'NoneType' object has no attribute '{}'", name), Some(*mpos)),
                        Dialect::Js => self.throw("TypeError", format!("Cannot read properties of {} (reading '{}')", self.display(&o), name), Some(*mpos)),
                        _ => self.throw("NullPointerException", format!("cannot call method {}() on null", name), Some(*mpos)),
                    };
                }
                Ok(Some(self.call_method(&o, name, a, *mpos)?))
            }
            Expr::Index { obj, index, .. } => {
                let o = match self.eval_chain(obj, env)? {
                    Some(o) => o,
                    None => return Ok(None),
                };
                let k = self.eval(index, env)?;
                let a = self.eval_args(args, env)?;
                if let V::Str(name) = &k {
                    if !matches!(o, V::Obj(_)) {
                        return Ok(Some(self.call_method(&o, name, a, pos)?));
                    }
                }
                let f = self.get_index(&o, &k, pos)?;
                Ok(Some(self.call(&f, o, a, pos)?))
            }
            Expr::Super(spos) => {
                let parent = env.lookup("%parent");
                let this = env.lookup("this").unwrap_or(V::Undef);
                let a = self.eval_args(args, env)?;
                match parent {
                    Some(V::Func(p)) => {
                        if p.flavor == Flavor::Class {
                            self.invoke_ctor_on(&p, &this, a, *spos)?;
                        } else {
                            self.call_closure(&p, this, a, *spos)?;
                        }
                        Ok(Some(V::Undef))
                    }
                    Some(V::Host(_)) => {
                        // a builtin exception base: super(message)
                        self.super_builtin(&this, "<init>", &a, *spos)?;
                        Ok(Some(V::Undef))
                    }
                    _ => self.throw("SyntaxError", "'super' keyword unexpected here".into(), Some(*spos)),
                }
            }
            Expr::Ident(name, ipos) => {
                let found = if self.d.groovy_like() { self.lookup_with_fields(name, env) } else { env.lookup(name) };
                if found.is_none() && self.d.groovy_like() {
                    // a method of `this` called by its bare name inside a class
                    if let Some(this @ V::Func(_)) = env.lookup("this") {
                        if let V::Func(c) = &this {
                            if c.flavor == Flavor::Class {
                                let mut cur = Some(this.clone());
                                let mut has = false;
                                while let Some(V::Func(k)) = cur {
                                    if k.flavor != Flavor::Class {
                                        break;
                                    }
                                    if k.props.borrow().get(name).is_some_and(|f| f.is_callable()) {
                                        has = true;
                                        break;
                                    }
                                    cur = k.parent.clone();
                                }
                                if has {
                                    let a = self.eval_args(args, env)?;
                                    return Ok(Some(self.call_method(&this, name, a, *ipos)?));
                                }
                            }
                        }
                    }
                    if let Some(this @ V::Obj(_)) = env.lookup("this") {
                        if crate::classes::class_of(&this).is_some() {
                            let has = self.user_method(&this, name).is_some() || crate::classes::class_of(&this).is_some_and(|c| c.props.borrow().get(name).is_some_and(|f| f.is_callable()));
                            if has {
                                let a = self.eval_args(args, env)?;
                                return Ok(Some(self.call_method(&this, name, a, *ipos)?));
                            }
                        }
                    }
                }
                let f = match found {
                    Some(f) => f,
                    None => match crate::builtins::script_function(self, name) {
                        Some(f) => f,
                        None => {
                            if self.d == Dialect::Groovy && !name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                                return self.throw("MissingMethodException", format!("No signature of method: Script1.{}() is applicable", name), Some(*ipos));
                            }
                            if matches!(self.d, Dialect::Ognl | Dialect::Mvel | Dialect::Spel) {
                                // root object method (e.g. property('foo'), header('x'))
                                let a = self.eval_args(args, env)?;
                                let root = V::host(Host::Root);
                                return Ok(Some(self.call_method(&root, name, a, *ipos)?));
                            }
                            self.lookup_ident(name, env, *ipos)?
                        }
                    },
                };
                if optional && f.is_nullish() {
                    return Ok(None);
                }
                let a = self.eval_args(args, env)?;
                if !f.is_callable() && !matches!(f, V::Host(_)) {
                    // Groovy: calling a non-callable variable
                    let msg = match self.d {
                        Dialect::Js => format!("{} is not a function", name),
                        Dialect::Python => format!("'{}' object is not callable", crate::builtins::py_type_name(&f)),
                        _ => format!("{} is not a function", name),
                    };
                    return self.type_error(msg, Some(pos));
                }
                let this = if self.d == Dialect::Js { V::Undef } else { env.lookup("this").unwrap_or(V::Undef) };
                Ok(Some(self.call(&f, this, a, pos)?))
            }
            other => {
                let f = match self.eval_chain(other, env)? {
                    Some(f) => f,
                    None => return Ok(None),
                };
                if optional && f.is_nullish() {
                    return Ok(None);
                }
                let a = self.eval_args(args, env)?;
                Ok(Some(self.call(&f, V::Undef, a, pos)?))
            }
        }
    }

    /// Evaluate an expression.
    pub fn eval(&mut self, e: &Expr, env: &Env) -> R<V> {
        match e {
            Expr::Undef => Ok(V::Undef),
            Expr::Null => Ok(V::Null),
            Expr::Bool(b) => Ok(V::Bool(*b)),
            Expr::Int(i) => Ok(V::Int(*i)),
            Expr::Num(f) => Ok(if self.d == Dialect::Js || self.d == Dialect::Jsonnet { norm_num(*f) } else { V::Num(*f) }),
            Expr::Str(s) => Ok(V::str(s)),
            Expr::Template(parts) => {
                let mut out = String::new();
                for p in parts {
                    match p {
                        TplPart::Lit(s) => out.push_str(s),
                        TplPart::Expr(e, spec, conv) => {
                            let v = self.eval(e, env)?;
                            let v = match conv {
                                Some('r') => V::string(crate::builtins::py_repr(self, &v)),
                                _ => v,
                            };
                            match spec {
                                Some(s) => {
                                    let r = crate::format::format_spec(self, &v, s);
                                    match r {
                                        Ok(s) => out.push_str(&s),
                                        Err(m) => return self.throw("ValueError", m, None),
                                    }
                                }
                                None => {
                                    let t = if matches!(v, V::Obj(_)) { self.to_str(&v)? } else { self.display(&v) };
                                    out.push_str(&t);
                                }
                            }
                        }
                    }
                }
                Ok(V::string(out))
            }
            Expr::Regex(p, f) => crate::builtins::make_regex(self, p, f, 0),
            Expr::Ident(n, pos) => self.lookup_ident(n, env, *pos),
            Expr::This => Ok(env.lookup("this").unwrap_or_else(|| self.undef())),
            Expr::Super(pos) => match env.lookup("%home") {
                Some(V::Obj(h)) => {
                    let p = h.borrow().proto.clone();
                    Ok(p.map(V::Obj).unwrap_or(V::Undef))
                }
                _ => self.throw("SyntaxError", "'super' keyword unexpected here".into(), Some(*pos)),
            },
            Expr::Array(items) => {
                let v = self.eval_args(items, env)?;
                Ok(V::arr(v))
            }
            Expr::Object(props) => {
                let mut pm = PropMap::default();
                let obj = new_obj_ref(Obj::new());
                for p in props {
                    match p {
                        Prop::KV(k, ve) => {
                            let key = match k {
                                PropKey::Name(n) => n.clone(),
                                PropKey::Computed(e) => {
                                    let kv = self.eval(e, env)?;
                                    self.key_string(&kv)
                                }
                            };
                            let v = self.eval(ve, env)?;
                            pm.set(&key, v);
                        }
                        Prop::Get(k, f) | Prop::Set(k, f) => {
                            let key = match k {
                                PropKey::Name(n) => n.clone(),
                                PropKey::Computed(e) => {
                                    let kv = self.eval(e, env)?;
                                    self.key_string(&kv)
                                }
                            };
                            let fv = self.make_closure(f.clone(), env);
                            let existing = pm.get(&key).cloned();
                            let acc = if matches!(p, Prop::Get(..)) { self.make_accessor(Some(fv), None, existing) } else { self.make_accessor(None, Some(fv), existing) };
                            pm.set(&key, acc);
                        }
                        Prop::Spread(se) => {
                            let v = self.eval(se, env)?;
                            match &v {
                                V::Obj(o) => {
                                    for (k, x) in o.borrow().props.iter() {
                                        pm.set(k, x.clone());
                                    }
                                }
                                V::Arr(a) => {
                                    for (i, x) in a.borrow().iter().enumerate() {
                                        pm.set(&i.to_string(), x.clone());
                                    }
                                }
                                V::Host(h) => {
                                    if let Some(V::Obj(o)) = crate::host::snapshot(self, h) {
                                        for (k, x) in o.borrow().props.iter() {
                                            pm.set(k, x.clone());
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                obj.borrow_mut().props = pm;
                Ok(V::Obj(obj))
            }
            Expr::Func(f) => Ok(self.make_closure(f.clone(), env)),
            Expr::Class(c) => self.make_class(c, env),
            Expr::Member { .. } | Expr::Index { .. } | Expr::Call { .. } => Ok(self.eval_chain(e, env)?.unwrap_or_else(|| self.undef())),
            Expr::Slice { obj, start, end, step, pos } => {
                let o = self.eval(obj, env)?;
                let s = match start {
                    Some(x) => Some(self.eval(x, env)?),
                    None => None,
                };
                let en = match end {
                    Some(x) => Some(self.eval(x, env)?),
                    None => None,
                };
                let st = match step {
                    Some(x) => Some(self.eval(x, env)?),
                    None => None,
                };
                crate::builtins::py_slice(self, &o, s, en, st, *pos)
            }
            Expr::New { callee, args, pos } => {
                let f = match &**callee {
                    Expr::Type(n) => match env.lookup(n) {
                        Some(c @ V::Func(_)) => c,
                        _ => crate::builtins::type_ns(n).unwrap_or_else(|| V::host(Host::Type(n.clone()))),
                    },
                    other => self.eval(other, env)?,
                };
                let a = self.eval_args(args, env)?;
                self.construct(&f, a, *pos)
            }
            Expr::Unary(op, x, pos) => {
                if *op == UnOp::Typeof {
                    if let Expr::Ident(n, _) = &**x {
                        if env.lookup(n).is_none() {
                            return Ok(V::str("undefined"));
                        }
                    }
                    let v = self.eval(x, env)?;
                    return Ok(V::str(crate::builtins::js_typeof(&v)));
                }
                if *op == UnOp::Delete {
                    return match &**x {
                        Expr::Member { obj, name, .. } => {
                            let o = self.eval(obj, env)?;
                            self.delete_key(&o, name);
                            Ok(V::Bool(true))
                        }
                        Expr::Index { obj, index, .. } => {
                            let o = self.eval(obj, env)?;
                            let k = self.eval(index, env)?;
                            let ks = self.key_string(&k);
                            self.delete_key(&o, &ks);
                            Ok(V::Bool(true))
                        }
                        _ => Ok(V::Bool(true)),
                    };
                }
                let v = self.eval(x, env)?;
                self.unary(*op, v, *pos)
            }
            Expr::Update { inc, prefix, target, pos } => {
                let old = self.eval(target, env)?;
                let big = crate::jsext::is_bigint(&old);
                let oldn = if self.d == Dialect::Js && !big { self.to_number(&old) } else { old.clone() };
                let one = if big { crate::jsext::make_bigint(crate::bigint::BigInt::from_i64(1)) } else { V::Int(1) };
                let new = self.binary(if *inc { BinOp::Add } else { BinOp::Sub }, oldn.clone(), one, *pos)?;
                let pat = match &**target {
                    Expr::Ident(n, _) => Pat::Id(n.clone()),
                    other => Pat::Target(other.clone()),
                };
                self.assign_pat(&pat, new.clone(), &Mode::Assign, env)?;
                Ok(if *prefix { new } else { oldn })
            }
            Expr::Bin(op, a, b, pos) => {
                let av = self.eval(a, env)?;
                let bv = self.eval(b, env)?;
                self.binary(*op, av, bv, *pos)
            }
            Expr::And(a, b) => {
                let av = self.eval(a, env)?;
                if !self.truthy(&av) {
                    return Ok(if self.returns_operand() { av } else { V::Bool(false) });
                }
                let bv = self.eval(b, env)?;
                Ok(if self.returns_operand() { bv } else { V::Bool(self.truthy(&bv)) })
            }
            Expr::Or(a, b) => {
                let av = self.eval(a, env)?;
                if self.truthy(&av) {
                    return Ok(if self.returns_operand() { av } else { V::Bool(true) });
                }
                let bv = self.eval(b, env)?;
                Ok(if self.returns_operand() { bv } else { V::Bool(self.truthy(&bv)) })
            }
            Expr::Nullish(a, b) => {
                let av = self.eval(a, env)?;
                if av.is_nullish() {
                    self.eval(b, env)
                } else {
                    Ok(av)
                }
            }
            Expr::Elvis(a, b) => {
                let av = self.eval(a, env)?;
                if self.truthy(&av) {
                    Ok(av)
                } else {
                    self.eval(b, env)
                }
            }
            Expr::Cond(c, a, b) => {
                let cv = self.eval(c, env)?;
                if self.truthy(&cv) {
                    self.eval(a, env)
                } else {
                    self.eval(b, env)
                }
            }
            Expr::Assign { op, target, value, pos } => {
                let v = match op {
                    None => match &**target {
                        Pat::Id(n) => self.eval_named(value, env, &Pat::Id(n.clone()))?,
                        _ => self.eval(value, env)?,
                    },
                    Some(aop) => {
                        let cur_expr = match &**target {
                            Pat::Id(n) => Expr::Ident(n.clone(), *pos),
                            Pat::Target(e) => e.clone(),
                            _ => return self.throw("SyntaxError", "invalid compound assignment".into(), Some(*pos)),
                        };
                        // evaluate the target object once
                        let (holder, key) = match &cur_expr {
                            Expr::Member { obj, name, .. } => (Some(self.eval(obj, env)?), Some(V::str(name))),
                            Expr::Index { obj, index, .. } => (Some(self.eval(obj, env)?), Some(self.eval(index, env)?)),
                            _ => (None, None),
                        };
                        let cur = match (&holder, &key) {
                            (Some(h), Some(k)) => {
                                if matches!(cur_expr, Expr::Member { .. }) {
                                    self.get_member(h, &self.key_string(k), *pos)?
                                } else {
                                    self.get_index(h, k, *pos)?
                                }
                            }
                            _ => self.eval(&cur_expr, env)?,
                        };
                        let nv = match aop {
                            AssignOp::Bin(b) => {
                                let rhs = self.eval(value, env)?;
                                // Python list += is in-place extend
                                if self.d == Dialect::Python && *b == BinOp::Add {
                                    if let V::Arr(a) = &cur {
                                        let items = self.iterate(&rhs, Some(*pos))?;
                                        a.borrow_mut().extend(items);
                                        return Ok(V::Null);
                                    }
                                }
                                self.binary(*b, cur, rhs, *pos)?
                            }
                            AssignOp::And => {
                                if self.truthy(&cur) {
                                    self.eval(value, env)?
                                } else {
                                    return Ok(cur);
                                }
                            }
                            AssignOp::Or | AssignOp::Elvis => {
                                if self.truthy(&cur) {
                                    return Ok(cur);
                                }
                                self.eval(value, env)?
                            }
                            AssignOp::Nullish => {
                                if !cur.is_nullish() {
                                    return Ok(cur);
                                }
                                self.eval(value, env)?
                            }
                        };
                        match (holder, key) {
                            (Some(h), Some(k)) => {
                                if matches!(cur_expr, Expr::Member { .. }) {
                                    let ks = self.key_string(&k);
                                    self.set_member(&h, &ks, nv.clone(), *pos)?;
                                } else {
                                    self.set_index(&h, &k, nv.clone(), *pos)?;
                                }
                                return Ok(nv);
                            }
                            _ => nv,
                        }
                    }
                };
                self.assign_pat(target, v.clone(), &Mode::Assign, env)?;
                Ok(v)
            }
            Expr::Seq(items) => {
                let mut last = self.undef();
                for x in items {
                    last = self.eval(x, env)?;
                }
                Ok(last)
            }
            Expr::Range { from, to, exclusive } => {
                let f = self.eval(from, env)?;
                let t = self.eval(to, env)?;
                crate::builtins::make_range(self, &f, &t, *exclusive)
            }
            Expr::As(x, t, pos) => {
                let v = self.eval(x, env)?;
                crate::builtins::as_type(self, v, t, *pos)
            }
            Expr::SpreadMember { obj, name } => {
                let o = self.eval(obj, env)?;
                if o.is_nullish() {
                    return Ok(V::Null);
                }
                let items = self.iterate(&o, None)?;
                let mut out = Vec::new();
                for it in items {
                    if it.is_nullish() {
                        out.push(V::Null);
                    } else {
                        out.push(self.get_member(&it, name, 0)?);
                    }
                }
                Ok(V::arr(out))
            }
            Expr::Project { obj, expr } => {
                let o = self.eval(obj, env)?;
                let items = self.iterate(&o, None)?;
                let mut out = Vec::new();
                for it in items {
                    let scope = new_scope(Some(env.clone()), false);
                    scope.define("#this", it.clone());
                    scope.define("this", it.clone());
                    out.push(self.eval(expr, &scope)?);
                }
                Ok(V::arr(out))
            }
            Expr::Select { obj, expr, mode } => {
                let o = self.eval(obj, env)?;
                let is_map = matches!(&o, V::Obj(ob) if ob.borrow().kind == ObjKind::Plain);
                let items = if is_map { self.iterate(&V::arr(vec![]), None).map(|_| crate::builtins::entries_of(&o))? } else { self.iterate(&o, None)? };
                let mut out = Vec::new();
                for it in items {
                    let scope = new_scope(Some(env.clone()), false);
                    scope.define("#this", it.clone());
                    scope.define("this", it.clone());
                    let r = self.eval(expr, &scope)?;
                    if self.truthy(&r) {
                        out.push(it);
                        if *mode == SelectMode::First {
                            break;
                        }
                    }
                }
                if is_map {
                    // selection on a map returns a map
                    let mut pm = PropMap::default();
                    for e in &out {
                        let k = self.get_member(e, "key", 0)?;
                        let v = self.get_member(e, "value", 0)?;
                        pm.set(&self.key_string(&k), v);
                    }
                    if *mode != SelectMode::All {
                        let last = if *mode == SelectMode::First { out.first() } else { out.last() };
                        return Ok(match last {
                            Some(e) => {
                                let k = self.get_member(e, "key", 0)?;
                                let v = self.get_member(e, "value", 0)?;
                                let mut one = PropMap::default();
                                one.set(&self.key_string(&k), v);
                                V::obj(one)
                            }
                            None => V::Null,
                        });
                    }
                    return Ok(V::obj(pm));
                }
                Ok(match mode {
                    SelectMode::All => V::arr(out),
                    SelectMode::First => out.into_iter().next().unwrap_or(V::Null),
                    SelectMode::Last => out.pop().unwrap_or(V::Null),
                })
            }
            Expr::Comp { kind, key, value, clauses } => {
                let scope = new_scope(Some(env.clone()), false);
                let mut out_list = Vec::new();
                let mut out_map = PropMap::default();
                self.comp_loop(clauses, 0, &scope, &mut |it: &mut Interp, sc: &Env| {
                    let k = it.eval(key, sc)?;
                    match value {
                        Some(ve) => {
                            let v = it.eval(ve, sc)?;
                            if it.d == Dialect::Jsonnet && !matches!(k, V::Str(_)) {
                                if k.is_nullish() {
                                    return Ok(());
                                }
                                return it.throw("RUNTIME ERROR", format!("field name must be string, got {}", crate::builtins::py_type_name(&k)), None);
                            }
                            let ks = it.key_string(&k);
                            out_map.set(&ks, v);
                        }
                        None => out_list.push(k),
                    }
                    Ok(())
                })?;
                Ok(match kind {
                    CompKind::List => V::arr(out_list),
                    CompKind::Dict => V::obj(out_map),
                    CompKind::Set => crate::builtins::make_set(self, out_list),
                })
            }
            Expr::Let(binds, body) => {
                let scope = new_scope(Some(env.clone()), false);
                for (n, x) in binds {
                    let v = self.eval(x, &scope)?;
                    scope.define(n, v);
                }
                self.eval(body, &scope)
            }
            Expr::Error(x, pos) => {
                let v = self.eval(x, env)?;
                let msg = self.display(&v);
                self.throw("RUNTIME ERROR", msg, Some(*pos))
            }
            Expr::Type(n) => match env.lookup(n) {
                Some(c @ V::Func(_)) => Ok(c),
                _ => Ok(crate::builtins::type_ns(n).unwrap_or_else(|| V::host(Host::Type(n.clone())))),
            },
            Expr::Yield { arg, delegate, pos } => self.eval_yield(arg.as_deref(), *delegate, env, *pos),
            Expr::Await(x, pos) => {
                if self.ystack.last().is_some_and(|c| c.async_gen) {
                    return self.eval_yield_marked(Some(x), false, env, *pos, true);
                }
                if !self.ystack.last().is_some_and(|c| c.is_async) {
                    return self.throw("SyntaxError", "await is only valid in async functions and the top level bodies of modules".into(), Some(*pos));
                }
                self.eval_yield(Some(x), false, env, *pos)
            }
            Expr::JObject(def) => self.eval_jobject(def, env),
            Expr::JSelf(k, pos) => {
                let name = match k {
                    0 => "self",
                    1 => "super",
                    _ => "$",
                };
                match env.lookup(name) {
                    Some(v) => Ok(v),
                    None => self.throw("STATIC ERROR", format!("can't use {} outside of an object", name), Some(*pos)),
                }
            }
            Expr::Import(kind, path, pos) => self.eval_import(*kind, path, *pos),
            Expr::SwitchExpr(disc, arms, pos) => {
                let dv = self.eval(disc, env)?;
                for (labels, body) in arms {
                    let mut hit = labels.is_empty();
                    for l in labels {
                        if let Expr::InstanceBind { ty, name, .. } = l {
                            if crate::builtins::is_type(&dv, ty) || matches!(env.lookup(ty), Some(V::Func(_)) if crate::builtins::instance_of(self, &dv, &env.lookup(ty).unwrap())) {
                                let scope = new_scope(Some(env.clone()), false);
                                scope.define(name, dv.clone());
                                return self.switch_arm(body, &scope);
                            }
                            continue;
                        }
                        let lv = self.eval(l, env)?;
                        if self.case_matches(&dv, &lv)? {
                            hit = true;
                            break;
                        }
                    }
                    if hit {
                        let scope = new_scope(Some(env.clone()), false);
                        return self.switch_arm(body, &scope);
                    }
                }
                self.throw("MatchException", format!("no switch case matches {}", self.display(&dv)), Some(*pos))
            }
            Expr::InstanceBind { expr, ty, name, pos } => {
                let v = self.eval(expr, env)?;
                let t = self.eval(&Expr::Type(ty.clone()), env)?;
                let ok = crate::builtins::instance_of(self, &v, &t);
                let _ = pos;
                if ok {
                    self.set_var(name, v, env);
                }
                Ok(V::Bool(ok))
            }
            Expr::MethodRef(target, name, _) => {
                let t = match &**target {
                    Expr::Ident(n, _) if env.lookup(n).is_none() && n.chars().next().is_some_and(|c| c.is_ascii_uppercase()) => {
                        crate::builtins::type_ns(n).unwrap_or_else(|| V::host(Host::Type(n.clone())))
                    }
                    other => self.eval(other, env)?,
                };
                Ok(V::Method(Rc::new((t, format!("%mref:{}", name)))))
            }
        }
    }

    /// Run a Java switch expression arm: an expression arm or a block ending with `yield`.
    fn switch_arm(&mut self, body: &[Stmt], env: &Env) -> R<V> {
        if let [Stmt::Expr(e)] = body {
            return self.eval(e, env);
        }
        match self.exec_block(body, env) {
            Ok(v) => Ok(v),
            Err(Ctrl::Value(v)) => Ok(v),
            Err(e) => Err(e),
        }
    }

    #[allow(clippy::type_complexity)]
    fn comp_loop(&mut self, clauses: &[CompClause], i: usize, env: &Env, emit: &mut dyn FnMut(&mut Interp, &Env) -> R<()>) -> R<()> {
        if i == clauses.len() {
            return emit(self, env);
        }
        match &clauses[i] {
            CompClause::For(pat, e) => {
                let v = self.eval(e, env)?;
                let items = self.iterate(&v, None)?;
                for it in items {
                    self.step()?;
                    let scope = new_scope(Some(env.clone()), false);
                    self.assign_pat(pat, it, &Mode::Declare(VarKind::Let), &scope)?;
                    self.comp_loop(clauses, i + 1, &scope, emit)?;
                }
                Ok(())
            }
            CompClause::If(c) => {
                let v = self.eval(c, env)?;
                if self.truthy(&v) {
                    self.comp_loop(clauses, i + 1, env, emit)
                } else {
                    Ok(())
                }
            }
        }
    }

    fn delete_key(&mut self, o: &V, k: &str) {
        if crate::jsext::is_proxy(o) {
            let _ = crate::jsext::proxy_delete(self, o, k, 0);
            return;
        }
        match o {
            V::Obj(ob) => {
                ob.borrow_mut().props.remove(k);
            }
            V::Arr(a) => {
                if let Ok(i) = k.parse::<usize>() {
                    if let Some(x) = a.borrow_mut().get_mut(i) {
                        *x = V::Undef;
                    }
                }
            }
            V::Host(h) => {
                let _ = crate::host::remove_key(self, h, k);
            }
            _ => {}
        }
    }

    fn returns_operand(&self) -> bool {
        matches!(self.d, Dialect::Js | Dialect::Python)
    }

    // ------------------------------------------------------------------ semantics
    /// Truthiness according to the dialect.
    pub fn truthy(&self, v: &V) -> bool {
        if let Some(b) = crate::jsext::bigint_of(v) {
            return !b.is_zero();
        }
        match v {
            V::Undef | V::Null => false,
            V::Bool(b) => *b,
            V::Int(i) => *i != 0,
            V::Num(f) => *f != 0.0 && !f.is_nan(),
            V::Str(s) => {
                if matches!(self.d, Dialect::Spel | Dialect::Ognl | Dialect::Mvel) && s.eq_ignore_ascii_case("false") {
                    return false;
                }
                !s.is_empty()
            }
            V::Bytes(b) => self.d == Dialect::Js || !b.is_empty(),
            V::Arr(a) => self.d == Dialect::Js || !a.borrow().is_empty(),
            V::Obj(o) => self.d == Dialect::Js || o.borrow().kind != ObjKind::Plain || !o.borrow().props.is_empty(),
            V::Host(h) => match &**h {
                Host::Headers | Host::Properties | Host::Variables if self.d != Dialect::Js => crate::host::snapshot(self, h).map(|s| self.truthy(&s)).unwrap_or(true),
                _ => true,
            },
            _ => true,
        }
    }

    /// JS ToNumber.
    pub fn to_number(&self, v: &V) -> V {
        match v {
            V::Int(_) | V::Num(_) => v.clone(),
            V::Undef => V::Num(f64::NAN),
            V::Null => V::Int(0),
            V::Bool(b) => V::Int(*b as i64),
            V::Str(s) => {
                let t = s.trim();
                if t.is_empty() {
                    return V::Int(0);
                }
                if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                    return i64::from_str_radix(h, 16).map(V::Int).unwrap_or(V::Num(f64::NAN));
                }
                match t {
                    "Infinity" | "+Infinity" => return V::Num(f64::INFINITY),
                    "-Infinity" => return V::Num(f64::NEG_INFINITY),
                    _ => {}
                }
                if t.chars().any(|c| c.is_ascii_alphabetic() && c != 'e' && c != 'E') {
                    return V::Num(f64::NAN);
                }
                t.parse::<i64>().map(V::Int).unwrap_or_else(|_| t.parse::<f64>().map(norm_num).unwrap_or(V::Num(f64::NAN)))
            }
            V::Arr(a) => {
                let a = a.borrow();
                match a.len() {
                    0 => V::Int(0),
                    1 => {
                        let x = a[0].clone();
                        drop(a);
                        self.to_number(&V::str(&self.display(&x)))
                    }
                    _ => V::Num(f64::NAN),
                }
            }
            V::Obj(o) => match o.borrow().kind {
                ObjKind::Date(ms) => norm_num(ms),
                _ => V::Num(f64::NAN),
            },
            _ => V::Num(f64::NAN),
        }
    }

    fn num_f(&self, v: &V) -> f64 {
        match self.to_number(v) {
            V::Int(i) => i as f64,
            V::Num(f) => f,
            _ => f64::NAN,
        }
    }

    /// Coerce an operand for arithmetic in non-JS dialects.
    fn arith_operand(&self, v: &V, op: &str, other: &V, pos: u32) -> R<V> {
        match v {
            V::Int(_) | V::Num(_) => Ok(v.clone()),
            V::Bool(b) if self.d == Dialect::Python => Ok(V::Int(*b as i64)),
            V::Str(s) if self.d.is_java() => match self.to_number(&V::Str(s.clone())) {
                V::Num(f) if f.is_nan() => self.type_error(format!("cannot apply '{}' to '{}'", op, s), Some(pos)),
                n => Ok(n),
            },
            _ => {
                let msg = match self.d {
                    Dialect::Python => format!(
                        "unsupported operand type(s) for {}: '{}' and '{}'",
                        op,
                        crate::builtins::py_type_name(v),
                        crate::builtins::py_type_name(other)
                    ),
                    Dialect::Groovy if v.is_nullish() => return self.throw("NullPointerException", format!("Cannot execute null{}{}", op, self.display(other)), Some(pos)),
                    Dialect::Groovy => format!("No signature of method: {}.{}() is applicable for argument types: ({})", crate::builtins::java_type(v), groovy_op_method(op), crate::builtins::java_type(other)),
                    _ => format!("operator '{}' cannot be applied to {} and {}", op, self.display_short(v), self.display_short(other)),
                };
                let name = if self.d == Dialect::Groovy { "MissingMethodException" } else { "TypeError" };
                if self.d == Dialect::Groovy || self.d == Dialect::Python {
                    return self.throw(name, msg, Some(pos));
                }
                self.type_error(msg, Some(pos))
            }
        }
    }

    fn int_or_float(&self, a: &V, b: &V, fi: fn(i64, i64) -> Option<i64>, ff: fn(f64, f64) -> f64) -> V {
        match (a, b) {
            (V::Int(x), V::Int(y)) => match fi(*x, *y) {
                Some(r) => V::Int(r),
                None => self.mk_num(ff(*x as f64, *y as f64)),
            },
            _ => self.mk_num(ff(a.as_f64().unwrap_or(f64::NAN), b.as_f64().unwrap_or(f64::NAN))),
        }
    }

    /// A float result in the dialect's representation.
    pub fn mk_num(&self, f: f64) -> V {
        if matches!(self.d, Dialect::Js | Dialect::Jsonnet) {
            norm_num(f)
        } else {
            V::Num(f)
        }
    }

    fn unary(&mut self, op: UnOp, v: V, pos: u32) -> R<V> {
        if let Some(b) = crate::jsext::bigint_of(&v) {
            if let Some(r) = crate::jsext::bigint_unary(self, op, &b, pos)? {
                return Ok(r);
            }
        }
        match op {
            UnOp::Not => Ok(V::Bool(!self.truthy(&v))),
            UnOp::Neg => {
                let n = if self.d == Dialect::Js { self.to_number(&v) } else { self.arith_operand(&v, "-", &V::Null, pos)? };
                Ok(match n {
                    V::Int(i) => match i.checked_neg() {
                        Some(x) if !(x == 0 && self.d == Dialect::Js) => V::Int(x),
                        _ => V::Num(-(i as f64)),
                    },
                    V::Num(f) => V::Num(-f),
                    other => other,
                })
            }
            UnOp::Plus => Ok(if self.d == Dialect::Js { self.to_number(&v) } else { v }),
            UnOp::BitNot => Ok(V::Int(!(self.num_f(&v) as i64))),
            UnOp::Void => Ok(V::Undef),
            UnOp::Typeof => Ok(V::str(crate::builtins::js_typeof(&v))),
            UnOp::Delete => Ok(V::Bool(true)),
        }
    }

    /// Apply a binary operator.
    pub fn binary(&mut self, op: BinOp, a: V, b: V, pos: u32) -> R<V> {
        use BinOp::*;
        let d = self.d;
        if d == Dialect::Js && (crate::jsext::is_bigint(&a) || crate::jsext::is_bigint(&b)) {
            if let Some(r) = crate::jsext::bigint_binary(self, op, &a, &b, pos)? {
                return Ok(r);
            }
        }
        if matches!(d, Dialect::Python | Dialect::Groovy | Dialect::Java) && (matches!(a, V::Obj(_)) || matches!(b, V::Obj(_))) {
            match op {
                Eq | Ne => {
                    if let Some(r) = self.user_equals(&a, &b, pos)? {
                        return Ok(V::Bool(if op == Eq { r } else { !r }));
                    }
                }
                _ => {
                    if let Some(r) = self.user_binary(op, &a, &b, pos)? {
                        return Ok(r);
                    }
                }
            }
        }
        match op {
            Add => self.add(a, b, pos),
            Sub | Mul | Div | Mod | Pow | FloorDiv => {
                if d == Dialect::Js || (d == Dialect::Jsonnet && !matches!(a, V::Str(_))) {
                    if d == Dialect::Jsonnet && (!a.is_number() || !b.is_number()) {
                        return self.throw("RUNTIME ERROR", format!("binary operator {} requires numbers, got {} and {}", op_symbol(op), crate::builtins::jsonnet_type(&a), crate::builtins::jsonnet_type(&b)), Some(pos));
                    }
                    let x = self.to_number(&a);
                    let y = self.to_number(&b);
                    return self.arith(op, x, y, pos);
                }
                // string / list operators
                match (op, &a, &b) {
                    (Mul, V::Str(s), V::Int(n)) | (Mul, V::Int(n), V::Str(s)) if matches!(d, Dialect::Groovy | Dialect::Python) => return Ok(V::string(s.repeat((*n).max(0) as usize))),
                    (Mul, V::Arr(x), V::Int(n)) if matches!(d, Dialect::Groovy | Dialect::Python) => {
                        let items = x.borrow().clone();
                        let mut out = Vec::new();
                        for _ in 0..(*n).max(0) {
                            out.extend(items.iter().cloned());
                        }
                        return Ok(V::arr(out));
                    }
                    (Sub, V::Str(s), _) if d == Dialect::Groovy => {
                        let r = self.display(&b);
                        return Ok(V::string(s.replacen(&r, "", 1)));
                    }
                    (Sub, V::Arr(x), _) if d == Dialect::Groovy => {
                        let remove = match &b {
                            V::Arr(y) => y.borrow().clone(),
                            other => vec![other.clone()],
                        };
                        let items = x.borrow().clone();
                        let out: Vec<V> = items.into_iter().filter(|it| !remove.iter().any(|r| self.equals(it, r))).collect();
                        return Ok(V::arr(out));
                    }
                    (Sub, V::Obj(x), _) if d == Dialect::Groovy || d == Dialect::Python => {
                        if let V::Obj(y) = &b {
                            let mut pm = x.borrow().props.clone();
                            for k in y.borrow().props.keys() {
                                pm.remove(k);
                            }
                            return Ok(V::obj(pm));
                        }
                    }
                    (Mod, V::Str(fmt), _) if matches!(d, Dialect::Python | Dialect::Jsonnet) => {
                        let args = match &b {
                            V::Arr(x) => x.borrow().clone(),
                            other => vec![other.clone()],
                        };
                        return match crate::format::printf(self, fmt, &args, Some(&b)) {
                            Ok(s) => Ok(V::string(s)),
                            Err(m) => self.throw(if d == Dialect::Python { "TypeError" } else { "RUNTIME ERROR" }, m, Some(pos)),
                        };
                    }
                    _ => {}
                }
                let sym = op_symbol(op);
                let x = self.arith_operand(&a, sym, &b, pos)?;
                let y = self.arith_operand(&b, sym, &a, pos)?;
                self.arith(op, x, y, pos)
            }
            Eq => Ok(V::Bool(self.equals(&a, &b))),
            Ne => Ok(V::Bool(!self.equals(&a, &b))),
            StrictEq => Ok(V::Bool(strict_eq(&a, &b))),
            StrictNe => Ok(V::Bool(!strict_eq(&a, &b))),
            Lt | Le | Gt | Ge => {
                let ord = self.compare(&a, &b);
                let ord = match ord {
                    Some(o) => o,
                    None => {
                        if d == Dialect::Python {
                            return self.throw(
                                "TypeError",
                                format!("'{}' not supported between instances of '{}' and '{}'", op_symbol(op), crate::builtins::py_type_name(&a), crate::builtins::py_type_name(&b)),
                                Some(pos),
                            );
                        }
                        if d == Dialect::Groovy && (a.is_nullish() || b.is_nullish()) {
                            // Groovy: null is smaller than everything
                            let o = if a.is_nullish() && b.is_nullish() { Ordering::Equal } else if a.is_nullish() { Ordering::Less } else { Ordering::Greater };
                            return Ok(V::Bool(match op {
                                Lt => o == Ordering::Less,
                                Le => o != Ordering::Greater,
                                Gt => o == Ordering::Greater,
                                _ => o != Ordering::Less,
                            }));
                        }
                        return Ok(V::Bool(false));
                    }
                };
                Ok(V::Bool(match op {
                    Lt => ord == Ordering::Less,
                    Le => ord != Ordering::Greater,
                    Gt => ord == Ordering::Greater,
                    _ => ord != Ordering::Less,
                }))
            }
            Cmp => Ok(V::Int(match self.compare(&a, &b) {
                Some(Ordering::Less) => -1,
                Some(Ordering::Equal) => 0,
                Some(Ordering::Greater) => 1,
                None => {
                    if a.is_nullish() && !b.is_nullish() {
                        -1
                    } else if b.is_nullish() && !a.is_nullish() {
                        1
                    } else {
                        0
                    }
                }
            })),
            BitAnd | BitOr | BitXor | Shl | Shr | UShr => {
                if op == Shl {
                    if let V::Arr(x) = &a {
                        if d == Dialect::Groovy || d == Dialect::Mvel {
                            x.borrow_mut().push(b);
                            return Ok(a);
                        }
                    }
                    if let V::Str(s) = &a {
                        if d == Dialect::Groovy {
                            return Ok(V::string(format!("{}{}", s, self.display(&b))));
                        }
                    }
                    if let (V::Obj(x), V::Obj(y)) = (&a, &b) {
                        let items: Vec<(Rc<str>, V)> = y.borrow().props.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                        for (k, v) in items {
                            x.borrow_mut().props.set(&k, v);
                        }
                        return Ok(a);
                    }
                }
                if matches!(op, BitAnd | BitOr | BitXor) {
                    if let (V::Bool(x), V::Bool(y)) = (&a, &b) {
                        return Ok(V::Bool(match op {
                            BitAnd => *x & *y,
                            BitOr => *x | *y,
                            _ => *x ^ *y,
                        }));
                    }
                    if d == Dialect::Python {
                        if let (V::Obj(_), V::Obj(_)) = (&a, &b) {
                            return crate::builtins::set_op(self, op, &a, &b);
                        }
                    }
                }
                let x = self.num_f(&a);
                let y = self.num_f(&b);
                if d == Dialect::Js {
                    let xi = (x as i64) as i32;
                    let yi = (y as i64) as i32;
                    return Ok(V::Int(match op {
                        BitAnd => (xi & yi) as i64,
                        BitOr => (xi | yi) as i64,
                        BitXor => (xi ^ yi) as i64,
                        Shl => xi.wrapping_shl(yi as u32 & 31) as i64,
                        Shr => (xi >> (yi as u32 & 31)) as i64,
                        _ => ((xi as u32) >> (yi as u32 & 31)) as i64,
                    }));
                }
                let (xi, yi) = (x as i64, y as i64);
                Ok(V::Int(match op {
                    BitAnd => xi & yi,
                    BitOr => xi | yi,
                    BitXor => xi ^ yi,
                    Shl => xi.wrapping_shl(yi as u32 & 63),
                    Shr => xi >> (yi as u32 & 63),
                    _ => ((xi as u64) >> (yi as u32 & 63)) as i64,
                }))
            }
            In | NotIn => {
                let r = self.contains(&b, &a, pos)?;
                Ok(V::Bool(if op == In { r } else { !r }))
            }
            Contains => Ok(V::Bool(self.contains(&a, &b, pos)?)),
            InstanceOf | NotInstanceOf => {
                let r = crate::builtins::instance_of(self, &a, &b);
                Ok(V::Bool(if op == InstanceOf { r } else { !r }))
            }
            Find => crate::builtins::groovy_find(self, &a, &b, pos),
            Matches => {
                let s = self.display(&a);
                let re = crate::builtins::to_regex(self, &b, pos)?;
                Ok(V::Bool(re.re.is_full_match(&s)))
            }
            Is => Ok(V::Bool(a.same(&b))),
            IsNot => Ok(V::Bool(!a.same(&b))),
            Between => {
                let items = self.iterate(&b, Some(pos))?;
                if items.len() != 2 {
                    return self.throw("SpelEvaluationException", "EL1040E: Right operand for the 'between' operator has to be a two-element list".into(), Some(pos));
                }
                let lo = self.compare(&a, &items[0]);
                let hi = self.compare(&a, &items[1]);
                Ok(V::Bool(matches!(lo, Some(Ordering::Greater | Ordering::Equal)) && matches!(hi, Some(Ordering::Less | Ordering::Equal))))
            }
        }
    }

    fn arith(&mut self, op: BinOp, x: V, y: V, pos: u32) -> R<V> {
        use BinOp::*;
        let d = self.d;
        Ok(match op {
            Sub => self.int_or_float(&x, &y, i64::checked_sub, |a, b| a - b),
            Mul => self.int_or_float(&x, &y, i64::checked_mul, |a, b| a * b),
            Div => {
                let yz = y.as_f64() == Some(0.0);
                match (d, &x, &y) {
                    (Dialect::Js, _, _) => self.mk_num(x.as_f64().unwrap_or(f64::NAN) / y.as_f64().unwrap_or(f64::NAN)),
                    (_, V::Int(_), V::Int(_)) | (Dialect::Python | Dialect::Jsonnet, _, _) if yz => {
                        let (name, msg) = match d {
                            Dialect::Python => ("ZeroDivisionError", "division by zero"),
                            Dialect::Jsonnet => ("RUNTIME ERROR", "division by zero"),
                            Dialect::Groovy => ("ArithmeticException", "Division by zero"),
                            _ => ("ArithmeticException", "/ by zero"),
                        };
                        return self.throw(name, msg.into(), Some(pos));
                    }
                    (Dialect::Ognl | Dialect::Mvel | Dialect::Spel | Dialect::Java, V::Int(a), V::Int(b)) => V::Int(a.wrapping_div(*b)),
                    (Dialect::Groovy, V::Int(a), V::Int(b)) if a % b == 0 => V::Int(a / b),
                    _ => self.mk_num(x.as_f64().unwrap_or(f64::NAN) / y.as_f64().unwrap_or(f64::NAN)),
                }
            }
            FloorDiv => {
                if y.as_f64() == Some(0.0) {
                    return self.throw("ZeroDivisionError", "integer division or modulo by zero".into(), Some(pos));
                }
                match (&x, &y) {
                    (V::Int(a), V::Int(b)) => V::Int(a.div_euclid(*b) - if (a.rem_euclid(*b) != 0) && (*b < 0) { 1 } else { 0 }),
                    _ => V::Num((x.as_f64().unwrap() / y.as_f64().unwrap()).floor()),
                }
            }
            Mod => {
                let yz = y.as_f64() == Some(0.0);
                match (&x, &y) {
                    (V::Int(_), V::Int(_)) if yz && d != Dialect::Js => {
                        let (name, msg) = match d {
                            Dialect::Python => ("ZeroDivisionError", "integer division or modulo by zero"),
                            Dialect::Jsonnet => ("RUNTIME ERROR", "division by zero"),
                            _ => ("ArithmeticException", "/ by zero"),
                        };
                        return self.throw(name, msg.into(), Some(pos));
                    }
                    (V::Int(a), V::Int(b)) if !yz => {
                        if d == Dialect::Python {
                            V::Int(((a % b) + b) % b)
                        } else {
                            V::Int(a.wrapping_rem(*b))
                        }
                    }
                    _ => {
                        let (a, b) = (x.as_f64().unwrap_or(f64::NAN), y.as_f64().unwrap_or(f64::NAN));
                        if d == Dialect::Python {
                            if b == 0.0 {
                                return self.throw("ZeroDivisionError", "float modulo".into(), Some(pos));
                            }
                            V::Num(a - b * (a / b).floor())
                        } else {
                            self.mk_num(a % b)
                        }
                    }
                }
            }
            Pow => match (&x, &y) {
                (V::Int(a), V::Int(b)) if *b >= 0 && *b <= u32::MAX as i64 => match a.checked_pow(*b as u32) {
                    Some(r) => V::Int(r),
                    None => self.mk_num((*a as f64).powf(*b as f64)),
                },
                _ => self.mk_num(x.as_f64().unwrap_or(f64::NAN).powf(y.as_f64().unwrap_or(f64::NAN))),
            },
            _ => V::Undef,
        })
    }

    fn add(&mut self, a: V, b: V, pos: u32) -> R<V> {
        let d = self.d;
        // string concatenation with an instance: user-defined toString()
        if d != Dialect::Jsonnet && d != Dialect::Python {
            let inst = |v: &V| matches!(v, V::Obj(o) if o.borrow().proto.is_some() && !matches!(o.borrow().kind, ObjKind::Error | ObjKind::Date(_)));
            if matches!(a, V::Str(_)) && inst(&b) {
                let t = self.to_str(&b)?;
                return Ok(V::string(format!("{}{}", self.display(&a), t)));
            }
            if matches!(b, V::Str(_)) && inst(&a) {
                let t = self.to_str(&a)?;
                return Ok(V::string(format!("{}{}", t, self.display(&b))));
            }
        }
        match d {
            Dialect::Js => {
                let prim = |v: &V| matches!(v, V::Str(_) | V::Arr(_) | V::Obj(_) | V::Func(_) | V::Regex(_) | V::Host(_) | V::Bytes(_));
                if prim(&a) || prim(&b) {
                    if let (V::Obj(o), _) | (_, V::Obj(o)) = (&a, &b) {
                        if let ObjKind::Date(_) = o.borrow().kind {}
                    }
                    return Ok(V::string(format!("{}{}", self.display(&a), self.display(&b))));
                }
                let x = self.to_number(&a);
                let y = self.to_number(&b);
                Ok(self.int_or_float(&x, &y, i64::checked_add, |p, q| p + q))
            }
            Dialect::Python => match (&a, &b) {
                (V::Str(x), V::Str(y)) => Ok(V::string(format!("{}{}", x, y))),
                (V::Arr(x), V::Arr(y)) => {
                    let mut v = x.borrow().clone();
                    v.extend(y.borrow().iter().cloned());
                    Ok(V::arr(v))
                }
                (V::Str(_), _) => self.throw("TypeError", format!("can only concatenate str (not \"{}\") to str", crate::builtins::py_type_name(&b)), Some(pos)),
                _ => {
                    let x = self.arith_operand(&a, "+", &b, pos)?;
                    let y = self.arith_operand(&b, "+", &a, pos)?;
                    Ok(self.int_or_float(&x, &y, i64::checked_add, |p, q| p + q))
                }
            },
            Dialect::Jsonnet => match (&a, &b) {
                (V::Str(_), _) | (_, V::Str(_)) => Ok(V::string(format!("{}{}", crate::builtins::jsonnet_to_string(self, &a), crate::builtins::jsonnet_to_string(self, &b)))),
                (V::Arr(x), V::Arr(y)) => {
                    let mut v = x.borrow().clone();
                    v.extend(y.borrow().iter().cloned());
                    Ok(V::arr(v))
                }
                (V::Obj(x), V::Obj(y)) => self.jobject_merge(x, y),
                _ if a.is_number() && b.is_number() => Ok(self.int_or_float(&a, &b, i64::checked_add, |p, q| p + q)),
                _ => self.throw("RUNTIME ERROR", format!("binary operator + does not operate on {} and {}", crate::builtins::jsonnet_type(&a), crate::builtins::jsonnet_type(&b)), Some(pos)),
            },
            _ => match (&a, &b) {
                (V::Str(x), _) => Ok(V::string(format!("{}{}", x, self.display(&b)))),
                (V::Arr(x), V::Arr(y)) if d != Dialect::Spel => {
                    let mut v = x.borrow().clone();
                    v.extend(y.borrow().iter().cloned());
                    Ok(V::arr(v))
                }
                (V::Arr(x), _) if d == Dialect::Groovy => {
                    let mut v = x.borrow().clone();
                    v.push(b.clone());
                    Ok(V::arr(v))
                }
                (V::Obj(x), V::Obj(y)) if d == Dialect::Groovy => Ok(merge_objects(x, y)),
                (_, V::Str(y)) if !matches!(a, V::Arr(_)) => Ok(V::string(format!("{}{}", self.display(&a), y))),
                _ => {
                    let x = self.arith_operand(&a, "+", &b, pos)?;
                    let y = self.arith_operand(&b, "+", &a, pos)?;
                    Ok(self.int_or_float(&x, &y, i64::checked_add, |p, q| p + q))
                }
            },
        }
    }

    /// Value equality according to the dialect (`==`).
    pub fn equals(&self, a: &V, b: &V) -> bool {
        match self.d {
            Dialect::Js => loose_eq(a, b),
            Dialect::Ognl | Dialect::Mvel => {
                if let (V::Str(s), n) | (n, V::Str(s)) = (a, b) {
                    if n.is_number() {
                        if let Ok(f) = s.trim().parse::<f64>() {
                            return Some(f) == n.as_f64();
                        }
                    }
                    if let V::Bool(x) = n {
                        return s.eq_ignore_ascii_case(if *x { "true" } else { "false" });
                    }
                }
                deep_eq(a, b)
            }
            _ => deep_eq(a, b),
        }
    }

    /// Ordering of two values (numbers, strings, arrays, dates).
    pub fn compare(&self, a: &V, b: &V) -> Option<Ordering> {
        match (a, b) {
            (V::Int(x), V::Int(y)) => Some(x.cmp(y)),
            (x, y) if x.is_number() && y.is_number() => x.as_f64()?.partial_cmp(&y.as_f64()?),
            (V::Str(x), V::Str(y)) => Some(x.cmp(y)),
            (V::Bool(x), V::Bool(y)) => Some(x.cmp(y)),
            (V::Arr(x), V::Arr(y)) if self.d != Dialect::Js => {
                let (x, y) = (x.borrow(), y.borrow());
                for (p, q) in x.iter().zip(y.iter()) {
                    match self.compare(p, q)? {
                        Ordering::Equal => continue,
                        o => return Some(o),
                    }
                }
                Some(x.len().cmp(&y.len()))
            }
            (V::Obj(x), V::Obj(y)) => match (&x.borrow().kind, &y.borrow().kind) {
                (ObjKind::Date(p), ObjKind::Date(q)) => p.partial_cmp(q),
                _ => None,
            },
            (V::Str(s), n) | (n, V::Str(s)) if n.is_number() && matches!(self.d, Dialect::Js | Dialect::Ognl | Dialect::Mvel) => {
                let f = if self.d == Dialect::Js { self.num_f(&V::Str(s.clone())) } else { s.trim().parse::<f64>().ok()? };
                let o = f.partial_cmp(&n.as_f64()?)?;
                Some(if matches!(a, V::Str(_)) { o } else { o.reverse() })
            }
            (V::Bool(_), _) | (_, V::Bool(_)) | (V::Null, _) | (_, V::Null) if self.d == Dialect::Js => {
                let x = self.num_f(a);
                let y = self.num_f(b);
                x.partial_cmp(&y)
            }
            _ => None,
        }
    }

    /// `needle in haystack`.
    pub fn contains(&mut self, haystack: &V, needle: &V, pos: u32) -> R<bool> {
        if crate::jsext::is_proxy(haystack) {
            return crate::jsext::proxy_has(self, haystack, needle, pos);
        }
        if let V::Obj(o) = haystack {
            if self.d == Dialect::Python && o.borrow().proto.is_some() {
                if let Some(r) = self.py_special(haystack, "__contains__", vec![needle.clone()], pos)? {
                    return Ok(self.truthy(&r));
                }
            }
            if matches!(o.borrow().kind, ObjKind::Jsonnet(_)) {
                let k = self.key_string(needle);
                return Ok(self.jhas(o, &k, true));
            }
            if let ObjKind::Coll(c) = o.borrow().kind.clone() {
                if let crate::collections::Coll::Set(m) | crate::collections::Coll::Map(m) = &mut *c.0.borrow_mut() {
                    if self.d != Dialect::Js {
                        return Ok(m.has(needle));
                    }
                }
            }
        }
        Ok(match haystack {
            V::Arr(a) => {
                if self.d == Dialect::Js {
                    let k = self.num_f(needle);
                    return Ok(k >= 0.0 && (k as usize) < a.borrow().len() || matches!(needle, V::Str(s) if &**s == "length"));
                }
                let items = a.borrow().clone();
                items.iter().any(|x| self.equals(x, needle))
            }
            V::Obj(o) => {
                let k = self.key_string(needle);
                let ob = o.borrow();
                if ob.kind == ObjKind::Set {
                    drop(ob);
                    let vals: Vec<V> = o.borrow().props.values().cloned().collect();
                    return Ok(vals.iter().any(|x| self.equals(x, needle)));
                }
                if self.d == Dialect::Js {
                    ob.lookup(&k).is_some()
                } else {
                    ob.props.contains(&k)
                }
            }
            V::Str(s) => {
                let n = self.display(needle);
                s.contains(n.as_str())
            }
            V::Host(h) => {
                let k = self.key_string(needle);
                crate::host::contains_key(self, h, &k)
            }
            V::Null | V::Undef if self.d != Dialect::Js && self.d != Dialect::Python => false,
            _ => {
                let msg = match self.d {
                    Dialect::Python => format!("argument of type '{}' is not iterable", crate::builtins::py_type_name(haystack)),
                    _ => format!("cannot use 'in' operator to search for '{}' in {}", self.display(needle), self.display_short(haystack)),
                };
                return self.type_error(msg, Some(pos));
            }
        })
    }

    /// A copy of `v` with own accessor properties (getters) evaluated, for conversion to Camel
    /// values and JSON (only copies when there is an accessor).
    pub fn materialize(&mut self, v: &V) -> R<V> {
        self.materialize_depth(v, 0)
    }

    fn materialize_depth(&mut self, v: &V, depth: usize) -> R<V> {
        if depth > 64 {
            return Ok(v.clone());
        }
        match v {
            V::Obj(o) => {
                if matches!(o.borrow().kind, ObjKind::Jsonnet(_)) {
                    self.jcomplete(o)?;
                    let items: Vec<V> = o.borrow().props.values().cloned().collect();
                    for x in items {
                        self.materialize_depth(&x, depth + 1)?;
                    }
                    return Ok(v.clone());
                }
                if o.borrow().kind != ObjKind::Plain {
                    return Ok(v.clone());
                }
                let items: Vec<(Rc<str>, V)> = o.borrow().props.iter().map(|(k, x)| (k.clone(), x.clone())).collect();
                let mut changed = false;
                let mut out = PropMap::default();
                for (k, x) in items {
                    let nx = match &x {
                        V::Obj(a) if a.borrow().kind == ObjKind::Accessor => {
                            let a = a.clone();
                            changed = true;
                            if a.borrow().props.get("get").is_none() {
                                continue;
                            }
                            self.call_getter(&a, v, 0)?
                        }
                        _ => x.clone(),
                    };
                    let nx = self.materialize_depth(&nx, depth + 1)?;
                    if !nx.same(&x) {
                        changed = true;
                    }
                    out.set(&k, nx);
                }
                Ok(if changed { V::obj(out) } else { v.clone() })
            }
            V::Arr(a) => {
                let items = a.borrow().clone();
                let mut changed = false;
                let mut out = Vec::with_capacity(items.len());
                for x in &items {
                    let nx = self.materialize_depth(x, depth + 1)?;
                    if !nx.same(x) {
                        changed = true;
                    }
                    out.push(nx);
                }
                Ok(if changed { V::arr(out) } else { v.clone() })
            }
            _ => Ok(v.clone()),
        }
    }

    // ------------------------------------------------------------------ display
    /// String conversion (`String(x)`, `str(x)`, `toString()`, template interpolation).
    pub fn display(&self, v: &V) -> String {
        crate::builtins::display(self, v, 0)
    }

    /// Short description of a value for error messages.
    pub fn display_short(&self, v: &V) -> String {
        let s = match v {
            V::Str(s) => format!("\"{}\"", s),
            V::Func(c) => format!("function {}", c.def.name.clone().unwrap_or_default()),
            other => self.display(other),
        };
        if s.chars().count() > 40 {
            format!("{}...", s.chars().take(40).collect::<String>())
        } else {
            s
        }
    }
}

fn groovy_op_method(op: &str) -> &'static str {
    match op {
        "+" => "plus",
        "-" => "minus",
        "*" => "multiply",
        "/" => "div",
        "%" => "mod",
        "**" => "power",
        _ => "operator",
    }
}

/// Operator symbol for error messages.
pub fn op_symbol(op: BinOp) -> &'static str {
    use BinOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Mod => "%",
        Pow => "**",
        FloorDiv => "//",
        Lt => "<",
        Le => "<=",
        Gt => ">",
        Ge => ">=",
        _ => "?",
    }
}

/// Whether an error named `name` is caught by a handler for `ty`.
pub fn error_is_a(name: &str, ty: &str) -> bool {
    if name == ty {
        return true;
    }
    match ty {
        "Exception" | "Throwable" | "BaseException" | "Error" | "RuntimeException" => true,
        "LookupError" => matches!(name, "KeyError" | "IndexError"),
        "ArithmeticError" => matches!(name, "ZeroDivisionError" | "OverflowError"),
        "IllegalArgumentException" => matches!(name, "NumberFormatException"),
        "IndexOutOfBoundsException" => matches!(name, "StringIndexOutOfBoundsException" | "ArrayIndexOutOfBoundsException"),
        "GroovyRuntimeException" => matches!(name, "MissingPropertyException" | "MissingMethodException"),
        _ => false,
    }
}

/// Merge two objects (right wins), keeping the left key order.
pub fn merge_objects(x: &ObjRef, y: &ObjRef) -> V {
    let mut pm = x.borrow().props.clone();
    for (k, v) in y.borrow().props.iter() {
        pm.set(k, v.clone());
    }
    V::obj(pm)
}

/// JS `===`.
pub fn strict_eq(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Undef, V::Undef) | (V::Null, V::Null) => true,
        (x, y) if x.is_number() && y.is_number() => x.as_f64() == y.as_f64(),
        (V::Str(x), V::Str(y)) => x == y,
        (V::Bool(x), V::Bool(y)) => x == y,
        _ => a.same(b) && !matches!(a, V::Undef | V::Null),
    }
}

/// JS `==`.
pub fn loose_eq(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Undef | V::Null, V::Undef | V::Null) => true,
        (V::Undef | V::Null, _) | (_, V::Undef | V::Null) => false,
        (x, y) if x.is_number() && y.is_number() => x.as_f64() == y.as_f64(),
        (V::Str(x), V::Str(y)) => x == y,
        (V::Bool(x), V::Bool(y)) => x == y,
        (V::Str(s), n) | (n, V::Str(s)) if n.is_number() || matches!(n, V::Bool(_)) => {
            let t = s.trim();
            let f = if t.is_empty() { Some(0.0) } else { t.parse::<f64>().ok() };
            let nf = match n {
                V::Bool(b) => Some(*b as i64 as f64),
                other => other.as_f64(),
            };
            f.is_some() && f == nf
        }
        (V::Bool(x), n) | (n, V::Bool(x)) if n.is_number() => n.as_f64() == Some(*x as i64 as f64),
        _ => a.same(b),
    }
}

/// Deep value equality (Groovy, Python, Java languages, Jsonnet).
pub fn deep_eq(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Undef | V::Null, V::Undef | V::Null) => true,
        (x, y) if x.is_number() && y.is_number() => x.as_f64() == y.as_f64(),
        (V::Str(x), V::Str(y)) => x == y,
        (V::Bool(x), V::Bool(y)) => x == y,
        (V::Bytes(x), V::Bytes(y)) => x == y,
        (V::Arr(x), V::Arr(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| deep_eq(p, q))
        }
        (V::Obj(x), V::Obj(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            if let (ObjKind::Date(p), ObjKind::Date(q)) = (&x.kind, &y.kind) {
                return p == q;
            }
            if x.kind == ObjKind::Set && y.kind == ObjKind::Set {
                return x.props.len() == y.props.len() && x.props.keys().all(|k| y.props.contains(k));
            }
            x.props.len() == y.props.len() && x.props.iter().all(|(k, v)| y.props.get(k).is_some_and(|w| deep_eq(v, w)))
        }
        _ => a.same(b),
    }
}
