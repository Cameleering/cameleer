//! Generators (JavaScript `function*`, Python functions with `yield`) and the resumable
//! executor that also runs `async` functions.
//!
//! The interpreter is a recursive tree walker; a generator body is instead executed by an
//! explicit frame stack ([`Frame`]) for its statements, so it can be suspended at a `yield` and
//! resumed later on the same thread, without threads or CPS. Expressions are still evaluated by
//! the recursive evaluator: a `yield` inside an expression suspends by unwinding with
//! [`Ctrl::Yield`]; when the generator resumes, the interrupted statement-level expression is
//! re-evaluated with the values of the yields it already passed replayed from a memo (so a
//! `yield` operand is evaluated once; other side effects of the enclosing expression that happen
//! before the `yield` are repeated, which only matters for code like `f() + (yield x)`).

use std::rc::Rc;

use crate::ast::*;
use crate::interp::{new_scope, Ctrl, Env, Interp, Mode, R};
use crate::value::*;

/// How a suspended generator is resumed.
#[derive(Clone)]
pub enum Resume {
    Next(V),
    Throw(V),
    Return(V),
}

/// Replay state of the statement-level expression being evaluated by a generator.
#[derive(Default)]
pub struct YieldCtx {
    pub memo: Vec<Resume>,
    pub cursor: usize,
    /// Active `yield*` delegate: (memo position, iterator).
    pub delegate: Option<(usize, V)>,
    /// Resume value for the active delegate.
    pub pending: Option<Resume>,
    pub is_async: bool,
    /// Async generators: `await` settles the promise synchronously (draining the job queues).
    pub sync_await: bool,
}

/// A statement pointer into the generator function's AST (kept alive by [`GenState::func`] /
/// [`GenState::body`]; the AST is immutable).
#[derive(Clone, Copy)]
struct SP(*const Stmt);
#[derive(Clone, Copy)]
struct LP(*const [Stmt]);

impl SP {
    fn get<'a>(self) -> &'a Stmt {
        // SAFETY: the pointee is part of an `Arc<FuncDef>` / `Arc<[Stmt]>` owned by the
        // `GenState` that owns this frame, and AST nodes are never mutated or moved.
        unsafe { &*self.0 }
    }
}

impl LP {
    fn get<'a>(self) -> &'a [Stmt] {
        // SAFETY: see `SP::get`.
        unsafe { &*self.0 }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Init,
    Test,
    Body,
    Running,
    Update,
}

enum ForIter {
    List(Vec<V>, usize),
    Lazy(V),
}

#[derive(Clone, Copy, PartialEq)]
enum TryPhase {
    Block,
    Catch,
    Finally,
}

enum Frame {
    Block { stmts: LP, i: usize, env: Env },
    Loop { stmt: SP, env: Env, label: Option<String>, phase: Phase },
    ForIn { stmt: SP, env: Env, label: Option<String>, iter: Option<ForIter>, phase: Phase },
    Try { stmt: SP, env: Env, phase: TryPhase, pending: Option<Completion> },
    Switch { stmt: SP, env: Env, case: usize, idx: usize },
    Label { label: String },
}

/// Completion of a statement.
enum Completion {
    Normal,
    Break(Option<String>),
    Continue(Option<String>),
    Return(V),
    Throw(V),
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Start,
    Suspended,
    Running,
    Done,
}

/// The state of a generator / async function activation.
#[derive(Default)]
pub struct GenState {
    pub func: Option<Rc<Closure>>,
    body: Option<std::sync::Arc<FuncDef>>,
    expr_env: Option<Env>,
    frames: Vec<Frame>,
    pub ctx: YieldCtx,
    pub status: Status,
    pub is_async: bool,
    /// Async functions: the promise of the result.
    pub promise: Option<V>,
    last: V,
    implicit_return: bool,
}

impl Default for V {
    fn default() -> Self {
        V::Undef
    }
}

enum Start {
    Done(Completion, V),
    Pushed,
    Suspend(V),
}

/// Result of running a generator until it yields or completes.
pub enum GenOut {
    Yielded(V),
    Returned(V),
}

fn completion_of(r: R<V>) -> Result<(Completion, V), Ctrl> {
    match r {
        Ok(v) => Ok((Completion::Normal, v)),
        Err(Ctrl::Break(l)) => Ok((Completion::Break(l), V::Undef)),
        Err(Ctrl::Continue(l)) => Ok((Completion::Continue(l), V::Undef)),
        Err(Ctrl::Return(v)) => Ok((Completion::Return(v), V::Undef)),
        Err(Ctrl::Throw(e)) => Ok((Completion::Throw(e), V::Undef)),
        Err(other) => Err(other),
    }
}

/// Whether a statement list contains `yield` / `await` outside nested functions.
pub fn stmts_have_yield(stmts: &[Stmt]) -> bool {
    stmts.iter().any(stmt_has_yield)
}

/// Whether a statement contains `yield` / `await` outside nested functions.
pub fn stmt_has_yield(s: &Stmt) -> bool {
    match s {
        Stmt::Expr(e) | Stmt::Throw(e, _) | Stmt::SwitchYield(e) => expr_has_yield(e),
        Stmt::Var(_, decls) => decls.iter().any(|(_, e)| e.as_ref().is_some_and(expr_has_yield)),
        Stmt::Return(e) => e.as_ref().is_some_and(expr_has_yield),
        Stmt::If(c, a, b) => expr_has_yield(c) || stmt_has_yield(a) || b.as_deref().is_some_and(stmt_has_yield),
        Stmt::Block(b) => stmts_have_yield(b),
        Stmt::For { init, test, update, body } => {
            init.as_deref().is_some_and(stmt_has_yield) || test.as_ref().is_some_and(expr_has_yield) || update.as_ref().is_some_and(expr_has_yield) || stmt_has_yield(body)
        }
        Stmt::ForIn { obj, body, else_body, .. } => expr_has_yield(obj) || stmt_has_yield(body) || else_body.as_deref().is_some_and(stmt_has_yield),
        Stmt::While(c, b, e) => expr_has_yield(c) || stmt_has_yield(b) || e.as_deref().is_some_and(stmt_has_yield),
        Stmt::DoWhile(b, c) => expr_has_yield(c) || stmt_has_yield(b),
        Stmt::Try { block, catches, finally } => stmts_have_yield(block) || catches.iter().any(|c| stmts_have_yield(&c.body)) || finally.as_deref().is_some_and(stmts_have_yield),
        Stmt::Switch(d, cases) => expr_has_yield(d) || cases.iter().any(|(t, b)| t.as_ref().is_some_and(expr_has_yield) || stmts_have_yield(b)),
        Stmt::Labeled(_, b) => stmt_has_yield(b),
        Stmt::Assert(e, m, _) => expr_has_yield(e) || m.as_ref().is_some_and(expr_has_yield),
        Stmt::Func(_) | Stmt::Class(_) | Stmt::Break(_) | Stmt::Continue(_) | Stmt::Empty | Stmt::Global(_) => false,
    }
}

fn elems_have_yield(items: &[Elem]) -> bool {
    items.iter().any(|e| match e {
        Elem::Item(x) | Elem::Spread(x) => expr_has_yield(x),
        Elem::Hole => false,
    })
}

/// Whether an expression contains `yield` / `await` outside nested functions.
pub fn expr_has_yield(e: &Expr) -> bool {
    match e {
        Expr::Yield { .. } | Expr::Await(..) => true,
        Expr::Template(parts) => parts.iter().any(|p| matches!(p, TplPart::Expr(x, _, _) if expr_has_yield(x))),
        Expr::Array(items) => elems_have_yield(items),
        Expr::Object(props) => props.iter().any(|p| match p {
            Prop::KV(k, v) => expr_has_yield(v) || matches!(k, PropKey::Computed(x) if expr_has_yield(x)),
            Prop::Spread(x) => expr_has_yield(x),
            Prop::Get(..) | Prop::Set(..) => false,
        }),
        Expr::Member { obj, .. } | Expr::SpreadMember { obj, .. } => expr_has_yield(obj),
        Expr::Index { obj, index, .. } => expr_has_yield(obj) || expr_has_yield(index),
        Expr::Slice { obj, start, end, step, .. } => {
            expr_has_yield(obj) || [start, end, step].iter().any(|x| x.as_deref().is_some_and(expr_has_yield))
        }
        Expr::Call { callee, args, .. } | Expr::New { callee, args, .. } => expr_has_yield(callee) || elems_have_yield(args),
        Expr::Unary(_, x, _) | Expr::As(x, _, _) | Expr::Error(x, _) | Expr::MethodRef(x, _, _) => expr_has_yield(x),
        Expr::InstanceBind { expr, .. } => expr_has_yield(expr),
        Expr::Update { target, .. } => expr_has_yield(target),
        Expr::Bin(_, a, b, _) | Expr::And(a, b) | Expr::Or(a, b) | Expr::Nullish(a, b) | Expr::Elvis(a, b) => expr_has_yield(a) || expr_has_yield(b),
        Expr::Cond(a, b, c) => expr_has_yield(a) || expr_has_yield(b) || expr_has_yield(c),
        Expr::Assign { target, value, .. } => expr_has_yield(value) || matches!(&**target, Pat::Target(x) if expr_has_yield(x)),
        Expr::Seq(items) => items.iter().any(expr_has_yield),
        Expr::Range { from, to, .. } => expr_has_yield(from) || expr_has_yield(to),
        Expr::Project { obj, expr } | Expr::Select { obj, expr, .. } => expr_has_yield(obj) || expr_has_yield(expr),
        Expr::Comp { .. } => false,
        Expr::Let(binds, body) => binds.iter().any(|(_, x)| expr_has_yield(x)) || expr_has_yield(body),
        Expr::SwitchExpr(d, arms, _) => expr_has_yield(d) || arms.iter().any(|(l, b)| l.iter().any(expr_has_yield) || stmts_have_yield(b)),
        _ => false,
    }
}

impl Interp {
    /// Create the generator object for a call of a generator (or async) function whose
    /// parameters are already bound in `scope`.
    pub fn make_generator(&mut self, c: &Rc<Closure>, scope: Env, is_async: bool) -> Internal<GenState> {
        let mut st = GenState { func: Some(c.clone()), is_async, implicit_return: c.def.implicit_return, ..Default::default() };
        st.ctx.is_async = is_async;
        st.ctx.sync_await = c.def.is_async && c.def.generator;
        if let FuncBody::Block(b) = &c.def.body {
            let lp = LP(b.as_slice() as *const [Stmt]);
            let env = self.block_env(b, &scope);
            st.frames.push(Frame::Block { stmts: lp, i: 0, env });
        } else if let FuncBody::Expr(_) = &c.def.body {
            // async arrow with an expression body: evaluate it as a return statement
            st.body = Some(c.def.clone());
            st.expr_env = Some(scope);
        }
        Internal::new(st)
    }

    /// The generator object value for a state.
    pub fn generator_value(&mut self, g: Internal<GenState>) -> V {
        let o = new_obj_ref(Obj { props: PropMap::default(), proto: None, kind: ObjKind::Generator(g) });
        V::Obj(o)
    }

    fn block_env(&mut self, b: &[Stmt], env: &Env) -> Env {
        let e = if self.d != Dialect::Python && crate::interp::decl_has_scope(b) { new_scope(Some(env.clone()), false) } else { env.clone() };
        self.hoist_pub(b, &e);
        e
    }

    /// Resume a generator; returns `(value, done)`.
    pub fn gen_resume(&mut self, g: &Internal<GenState>, r: Resume) -> R<(V, bool)> {
        let status = g.0.borrow().status;
        match status {
            Status::Running => return self.type_error("Generator is already running".into(), None),
            Status::Done => {
                return match r {
                    Resume::Next(_) => Ok((self.undef(), true)),
                    Resume::Throw(e) => Err(Ctrl::Throw(e)),
                    Resume::Return(v) => Ok((v, true)),
                }
            }
            Status::Start => match r {
                Resume::Throw(e) => {
                    g.0.borrow_mut().status = Status::Done;
                    return Err(Ctrl::Throw(e));
                }
                Resume::Return(v) => {
                    g.0.borrow_mut().status = Status::Done;
                    return Ok((v, true));
                }
                Resume::Next(_) => {}
            },
            Status::Suspended => {
                let mut b = g.0.borrow_mut();
                if b.ctx.delegate.is_some() {
                    b.ctx.pending = Some(r);
                } else {
                    b.ctx.memo.push(r);
                }
            }
        }
        // take the state out so that the body may access the generator object itself
        let mut st = std::mem::take(&mut *g.0.borrow_mut());
        g.0.borrow_mut().status = Status::Running;
        st.status = Status::Running;
        let ctx = std::mem::take(&mut st.ctx);
        self.ystack.push(ctx);
        let r = self.gen_run(&mut st);
        st.ctx = self.ystack.pop().unwrap_or_default();
        let out = match r {
            Ok(GenOut::Yielded(v)) => {
                st.status = Status::Suspended;
                Ok((v, false))
            }
            Ok(GenOut::Returned(v)) => {
                st.status = Status::Done;
                st.frames.clear();
                Ok((v, true))
            }
            Err(e) => {
                st.status = Status::Done;
                st.frames.clear();
                Err(e)
            }
        };
        *g.0.borrow_mut() = st;
        out
    }

    /// Evaluate a statement-level "point" with yield replay.
    fn point<T>(&mut self, f: impl FnOnce(&mut Interp) -> R<T>) -> Result<Result<T, V>, Ctrl> {
        if let Some(c) = self.ystack.last_mut() {
            c.cursor = 0;
        }
        match f(self) {
            Ok(v) => {
                self.clear_memo();
                Ok(Ok(v))
            }
            Err(Ctrl::Yield(v)) => Ok(Err(v)),
            Err(e) => {
                self.clear_memo();
                Err(e)
            }
        }
    }

    fn clear_memo(&mut self) {
        if let Some(c) = self.ystack.last_mut() {
            c.memo.clear();
            c.cursor = 0;
            c.delegate = None;
            c.pending = None;
        }
    }

    fn start_stmt(&mut self, st: &mut GenState, s: &Stmt, env: &Env, label: Option<String>) -> R<Start> {
        if !self.has_yield_cached(s) {
            let r = self.exec_labeled_pub(s, env, label.as_deref());
            let (c, v) = completion_of(r)?;
            return Ok(Start::Done(c, v));
        }
        let sp = SP(s as *const Stmt);
        match s {
            Stmt::Block(b) => {
                let e = self.block_env(b, env);
                st.frames.push(Frame::Block { stmts: LP(b.as_slice() as *const [Stmt]), i: 0, env: e });
                Ok(Start::Pushed)
            }
            Stmt::If(c, a, b) => {
                let r = self.point(|it| {
                    let v = it.eval(c, env)?;
                    Ok(it.truthy(&v))
                });
                match r {
                    Ok(Ok(t)) => {
                        let branch: Option<&Stmt> = if t { Some(a) } else { b.as_deref() };
                        if let Some(br) = branch {
                            st.frames.push(Frame::Block { stmts: LP(std::slice::from_ref(br) as *const [Stmt]), i: 0, env: env.clone() });
                            Ok(Start::Pushed)
                        } else {
                            Ok(Start::Done(Completion::Normal, V::Undef))
                        }
                    }
                    Ok(Err(y)) => Ok(Start::Suspend(y)),
                    Err(e) => completion_of(Err(e)).map(|(c, v)| Start::Done(c, v)),
                }
            }
            Stmt::While(..) => {
                st.frames.push(Frame::Loop { stmt: sp, env: env.clone(), label, phase: Phase::Test });
                Ok(Start::Pushed)
            }
            Stmt::DoWhile(..) => {
                st.frames.push(Frame::Loop { stmt: sp, env: env.clone(), label, phase: Phase::Body });
                Ok(Start::Pushed)
            }
            Stmt::For { .. } => {
                let scope = new_scope(Some(env.clone()), false);
                st.frames.push(Frame::Loop { stmt: sp, env: scope, label, phase: Phase::Init });
                Ok(Start::Pushed)
            }
            Stmt::ForIn { .. } => {
                st.frames.push(Frame::ForIn { stmt: sp, env: env.clone(), label, iter: None, phase: Phase::Init });
                Ok(Start::Pushed)
            }
            Stmt::Try { block, .. } => {
                st.frames.push(Frame::Try { stmt: sp, env: env.clone(), phase: TryPhase::Block, pending: None });
                let e = self.block_env(block, env);
                st.frames.push(Frame::Block { stmts: LP(block.as_slice() as *const [Stmt]), i: 0, env: e });
                Ok(Start::Pushed)
            }
            Stmt::Switch(disc, cases) => {
                let scope = new_scope(Some(env.clone()), false);
                let r = self.point(|it| {
                    let dv = it.eval(disc, &scope)?;
                    for (i, (test, _)) in cases.iter().enumerate() {
                        if let Some(t) = test {
                            let tv = it.eval(t, &scope)?;
                            if it.case_matches_pub(&dv, &tv)? {
                                return Ok(Some(i));
                            }
                        }
                    }
                    Ok(cases.iter().position(|(t, _)| t.is_none()))
                });
                match r {
                    Ok(Ok(Some(i))) => {
                        st.frames.push(Frame::Switch { stmt: sp, env: scope, case: i, idx: 0 });
                        Ok(Start::Pushed)
                    }
                    Ok(Ok(None)) => Ok(Start::Done(Completion::Normal, V::Undef)),
                    Ok(Err(y)) => Ok(Start::Suspend(y)),
                    Err(e) => completion_of(Err(e)).map(|(c, v)| Start::Done(c, v)),
                }
            }
            Stmt::Labeled(l, body) => {
                st.frames.push(Frame::Label { label: l.clone() });
                let r = self.start_stmt(st, body, env, Some(l.clone()))?;
                if matches!(r, Start::Suspend(_)) {
                    st.frames.pop();
                }
                Ok(r)
            }
            _ => {
                let r = self.point(|it| it.exec_labeled_pub(s, env, label.as_deref()));
                match r {
                    Ok(Ok(v)) => Ok(Start::Done(Completion::Normal, v)),
                    Ok(Err(y)) => Ok(Start::Suspend(y)),
                    Err(e) => completion_of(Err(e)).map(|(c, v)| Start::Done(c, v)),
                }
            }
        }
    }

    fn has_yield_cached(&mut self, s: &Stmt) -> bool {
        let k = s as *const Stmt as usize;
        if let Some(b) = self.yield_cache.get(&k) {
            return *b;
        }
        let b = stmt_has_yield(s);
        self.yield_cache.insert(k, b);
        b
    }

    fn gen_run(&mut self, st: &mut GenState) -> R<GenOut> {
        // async arrow with an expression body
        if let Some(def) = st.body.clone() {
            if let FuncBody::Expr(e) = &def.body {
                let env = st.expr_env.clone().unwrap_or_else(|| self.globals.clone());
                let r = self.point(|it| it.eval(e, &env));
                return match r {
                    Ok(Ok(v)) => {
                        st.body = None;
                        Ok(GenOut::Returned(v))
                    }
                    Ok(Err(y)) => Ok(GenOut::Yielded(y)),
                    Err(e) => Err(e),
                };
            }
        }
        let mut pending: Option<Completion> = None;
        loop {
            self.step_pub()?;
            if let Some(c) = pending.take() {
                match self.propagate(st, c)? {
                    Some(GenOutOrC::Out(o)) => return Ok(o),
                    Some(GenOutOrC::C(c)) => pending = Some(c),
                    None => {}
                }
                continue;
            }
            let Some(top) = st.frames.last_mut() else {
                let v = if st.implicit_return { std::mem::take(&mut st.last) } else { self.undef() };
                return Ok(GenOut::Returned(v));
            };
            match top {
                Frame::Block { stmts, i, env } => {
                    let list = stmts.get();
                    if *i >= list.len() {
                        st.frames.pop();
                        pending = Some(Completion::Normal);
                        continue;
                    }
                    let s = &list[*i];
                    let env = env.clone();
                    let idx = *i;
                    match self.start_stmt(st, s, &env, None)? {
                        Start::Suspend(y) => return Ok(GenOut::Yielded(y)),
                        Start::Done(c, v) => {
                            self.advance_block(st, idx, s);
                            if matches!(c, Completion::Normal) {
                                if matches!(s, Stmt::Expr(_)) {
                                    st.last = v;
                                }
                            } else {
                                pending = Some(c);
                            }
                        }
                        Start::Pushed => self.advance_block(st, idx, s),
                    }
                }
                Frame::Loop { stmt, env, phase, .. } => {
                    let s = stmt.get();
                    let env = env.clone();
                    let ph = *phase;
                    match (s, ph) {
                        (Stmt::For { init, .. }, Phase::Init) => {
                            if let Some(i) = init {
                                match self.point(|it| it.exec(i, &env)) {
                                    Ok(Ok(_)) => {}
                                    Ok(Err(y)) => return Ok(GenOut::Yielded(y)),
                                    Err(e) => {
                                        pending = Some(completion_of(Err(e))?.0);
                                        st.frames.pop();
                                        continue;
                                    }
                                }
                            }
                            set_phase(st, Phase::Test);
                        }
                        (_, Phase::Test) => {
                            let test = match s {
                                Stmt::While(c, _, _) | Stmt::DoWhile(_, c) => Some(c),
                                Stmt::For { test, .. } => test.as_ref(),
                                _ => None,
                            };
                            let ok = match test {
                                Some(t) => match self.point(|it| {
                                    let v = it.eval(t, &env)?;
                                    Ok(it.truthy(&v))
                                }) {
                                    Ok(Ok(b)) => b,
                                    Ok(Err(y)) => return Ok(GenOut::Yielded(y)),
                                    Err(e) => {
                                        st.frames.pop();
                                        pending = Some(completion_of(Err(e))?.0);
                                        continue;
                                    }
                                },
                                None => true,
                            };
                            if ok {
                                set_phase(st, Phase::Body);
                            } else {
                                st.frames.pop();
                                if let Stmt::While(_, _, Some(eb)) = s {
                                    st.frames.push(Frame::Block { stmts: LP(std::slice::from_ref(&**eb) as *const [Stmt]), i: 0, env: env.clone() });
                                }
                                pending = Some(Completion::Normal);
                            }
                        }
                        (_, Phase::Body) => {
                            let body: &Stmt = match s {
                                Stmt::While(_, b, _) | Stmt::DoWhile(b, _) => b,
                                Stmt::For { body, .. } => body,
                                _ => unreachable!(),
                            };
                            set_phase(st, Phase::Running);
                            st.frames.push(Frame::Block { stmts: LP(std::slice::from_ref(body) as *const [Stmt]), i: 0, env: env.clone() });
                        }
                        (Stmt::For { update, init, .. }, Phase::Update) => {
                            // fresh binding per iteration for `let` loops
                            let mut env = env;
                            if matches!(init.as_deref(), Some(Stmt::Var(VarKind::Let | VarKind::Const, _))) {
                                let fresh = new_scope(env.parent.clone(), false);
                                for (k, v) in env.vars.borrow().iter() {
                                    fresh.vars.borrow_mut().insert(k.clone(), v.clone());
                                }
                                env = fresh;
                                if let Some(Frame::Loop { env: e, .. }) = st.frames.last_mut() {
                                    *e = env.clone();
                                }
                            }
                            if let Some(u) = update {
                                match self.point(|it| it.eval(u, &env)) {
                                    Ok(Ok(_)) => {}
                                    Ok(Err(y)) => return Ok(GenOut::Yielded(y)),
                                    Err(e) => {
                                        st.frames.pop();
                                        pending = Some(completion_of(Err(e))?.0);
                                        continue;
                                    }
                                }
                            }
                            set_phase(st, Phase::Test);
                        }
                        _ => set_phase(st, Phase::Test),
                    }
                }
                Frame::ForIn { stmt, env, iter, phase, .. } => {
                    let s = stmt.get();
                    let env = env.clone();
                    let Stmt::ForIn { decl, pat, obj, body, values, else_body } = s else { unreachable!() };
                    if *phase == Phase::Init {
                        let r = self.point(|it| {
                            let o = it.eval(obj, &env)?;
                            if *values {
                                if let Some(i) = it.lazy_iterator(&o)? {
                                    return Ok(ForIter::Lazy(i));
                                }
                                Ok(ForIter::List(it.iterate(&o, None)?, 0))
                            } else {
                                Ok(ForIter::List(it.keys_of_pub(&o), 0))
                            }
                        });
                        match r {
                            Ok(Ok(i)) => {
                                *iter = Some(i);
                                *phase = Phase::Test;
                            }
                            Ok(Err(y)) => return Ok(GenOut::Yielded(y)),
                            Err(e) => {
                                st.frames.pop();
                                pending = Some(completion_of(Err(e))?.0);
                            }
                        }
                        continue;
                    }
                    // next item
                    let next = match iter {
                        Some(ForIter::List(items, i)) => {
                            let x = items.get(*i).cloned();
                            *i += 1;
                            Ok(x)
                        }
                        Some(ForIter::Lazy(itv)) => {
                            let itv = itv.clone();
                            self.iter_next(&itv)
                        }
                        None => Ok(None),
                    };
                    match next {
                        Err(e) => {
                            st.frames.pop();
                            pending = Some(completion_of(Err(e))?.0);
                        }
                        Ok(None) => {
                            st.frames.pop();
                            if let Some(eb) = else_body {
                                st.frames.push(Frame::Block { stmts: LP(std::slice::from_ref(&**eb) as *const [Stmt]), i: 0, env: env.clone() });
                            }
                            pending = Some(Completion::Normal);
                        }
                        Ok(Some(item)) => {
                            if let Some(Frame::ForIn { phase, .. }) = st.frames.last_mut() {
                                *phase = Phase::Running;
                            }
                            let scope = if self.d == Dialect::Python { env.clone() } else { new_scope(Some(env.clone()), false) };
                            let mode = match decl {
                                Some(k) => Mode::Declare(*k),
                                None => Mode::Assign,
                            };
                            if let Err(e) = self.assign_pat_pub(pat, item, &mode, &scope) {
                                st.frames.pop();
                                pending = Some(completion_of(Err(e))?.0);
                                continue;
                            }
                            st.frames.push(Frame::Block { stmts: LP(std::slice::from_ref(&**body) as *const [Stmt]), i: 0, env: scope });
                        }
                    }
                }
                Frame::Switch { stmt, env, case, idx } => {
                    let Stmt::Switch(_, cases) = stmt.get() else { unreachable!() };
                    if *case >= cases.len() {
                        st.frames.pop();
                        pending = Some(Completion::Normal);
                        continue;
                    }
                    if *idx >= cases[*case].1.len() {
                        *case += 1;
                        *idx = 0;
                        continue;
                    }
                    let s = &cases[*case].1[*idx];
                    let env = env.clone();
                    match self.start_stmt(st, s, &env, None)? {
                        Start::Suspend(y) => return Ok(GenOut::Yielded(y)),
                        Start::Done(c, _) => {
                            bump_switch(st, s);
                            if !matches!(c, Completion::Normal) {
                                pending = Some(c);
                            }
                        }
                        Start::Pushed => bump_switch(st, s),
                    }
                }
                Frame::Try { .. } | Frame::Label { .. } => {
                    // only reached when the child frame completed: handled by propagate
                    pending = Some(Completion::Normal);
                }
            }
        }
    }

    /// Advance the block frame that started statement `s` at index `idx` (it may no longer be
    /// on top: the statement may have pushed frames).
    fn advance_block(&mut self, st: &mut GenState, idx: usize, s: &Stmt) {
        for f in st.frames.iter_mut().rev() {
            if let Frame::Block { stmts, i, .. } = f {
                if *i == idx && stmts.get().get(idx).is_some_and(|x| std::ptr::eq(x, s)) {
                    *i += 1;
                    return;
                }
            }
        }
    }

    fn propagate(&mut self, st: &mut GenState, c: Completion) -> R<Option<GenOutOrC>> {
        let Some(top) = st.frames.last_mut() else {
            return match c {
                Completion::Normal | Completion::Break(_) | Completion::Continue(_) => {
                    let v = if st.implicit_return { std::mem::take(&mut st.last) } else { self.undef() };
                    Ok(Some(GenOutOrC::Out(GenOut::Returned(v))))
                }
                Completion::Return(v) => Ok(Some(GenOutOrC::Out(GenOut::Returned(v)))),
                Completion::Throw(e) => Err(Ctrl::Throw(e)),
            };
        };
        match top {
            Frame::Block { .. } => {
                if matches!(c, Completion::Normal) {
                    return Ok(None);
                }
                st.frames.pop();
                Ok(Some(GenOutOrC::C(c)))
            }
            Frame::Loop { label, phase, stmt, .. } => match c {
                Completion::Normal => {
                    *phase = if matches!(stmt.get(), Stmt::For { .. }) { Phase::Update } else { Phase::Test };
                    Ok(None)
                }
                Completion::Continue(ref l) if l.is_none() || l == label => {
                    *phase = if matches!(stmt.get(), Stmt::For { .. }) { Phase::Update } else { Phase::Test };
                    Ok(None)
                }
                Completion::Break(ref l) if l.is_none() || l == label => {
                    st.frames.pop();
                    Ok(Some(GenOutOrC::C(Completion::Normal)))
                }
                other => {
                    st.frames.pop();
                    Ok(Some(GenOutOrC::C(other)))
                }
            },
            Frame::ForIn { label, phase, .. } => match c {
                Completion::Normal => {
                    *phase = Phase::Test;
                    Ok(None)
                }
                Completion::Continue(ref l) if l.is_none() || l == label => {
                    *phase = Phase::Test;
                    Ok(None)
                }
                Completion::Break(ref l) if l.is_none() || l == label => {
                    st.frames.pop();
                    Ok(Some(GenOutOrC::C(Completion::Normal)))
                }
                other => {
                    st.frames.pop();
                    Ok(Some(GenOutOrC::C(other)))
                }
            },
            Frame::Switch { .. } => match c {
                Completion::Normal => Ok(None),
                Completion::Break(None) => {
                    st.frames.pop();
                    Ok(Some(GenOutOrC::C(Completion::Normal)))
                }
                other => {
                    st.frames.pop();
                    Ok(Some(GenOutOrC::C(other)))
                }
            },
            Frame::Label { label } => {
                let l = label.clone();
                st.frames.pop();
                match c {
                    Completion::Break(Some(x)) if x == l => Ok(Some(GenOutOrC::C(Completion::Normal))),
                    other => Ok(Some(GenOutOrC::C(other))),
                }
            }
            Frame::Try { stmt, env, phase, pending } => {
                let Stmt::Try { catches, finally, .. } = stmt.get() else { unreachable!() };
                let env = env.clone();
                match (*phase, c) {
                    (TryPhase::Block, Completion::Throw(exc)) if !catches.is_empty() => {
                        let mut found = None;
                        for cl in catches {
                            if self.catch_matches_pub(&cl.types, &exc) {
                                found = Some(cl);
                                break;
                            }
                        }
                        match found {
                            Some(cl) => {
                                *phase = TryPhase::Catch;
                                let scope = if self.d == Dialect::Python { env.clone() } else { new_scope(Some(env.clone()), false) };
                                if let Some(p) = &cl.param {
                                    let mode = if self.d == Dialect::Python { Mode::Assign } else { Mode::Declare(VarKind::Let) };
                                    self.assign_pat_pub(p, exc.clone(), &mode, &scope)?;
                                }
                                self.exc_stack.push(exc);
                                st.frames.push(Frame::Block { stmts: LP(cl.body.as_slice() as *const [Stmt]), i: 0, env: scope });
                                Ok(None)
                            }
                            None => self.enter_finally(st, finally, env, Completion::Throw(exc)),
                        }
                    }
                    (TryPhase::Block, c) => self.enter_finally(st, finally, env, c),
                    (TryPhase::Catch, c) => {
                        self.exc_stack.pop();
                        self.enter_finally(st, finally, env, c)
                    }
                    (TryPhase::Finally, Completion::Normal) => {
                        let p = pending.take();
                        st.frames.pop();
                        Ok(Some(GenOutOrC::C(p.unwrap_or(Completion::Normal))))
                    }
                    (TryPhase::Finally, other) => {
                        st.frames.pop();
                        Ok(Some(GenOutOrC::C(other)))
                    }
                }
            }
        }
    }

    fn enter_finally(&mut self, st: &mut GenState, finally: &Option<Vec<Stmt>>, env: Env, c: Completion) -> R<Option<GenOutOrC>> {
        match finally {
            Some(f) => {
                if let Some(Frame::Try { phase, pending, .. }) = st.frames.last_mut() {
                    *phase = TryPhase::Finally;
                    *pending = if matches!(c, Completion::Normal) { None } else { Some(c) };
                }
                let e = self.block_env(f, &env);
                st.frames.push(Frame::Block { stmts: LP(f.as_slice() as *const [Stmt]), i: 0, env: e });
                Ok(None)
            }
            None => {
                st.frames.pop();
                if matches!(c, Completion::Normal) {
                    // the try statement completed normally
                    return Ok(Some(GenOutOrC::C(Completion::Normal)));
                }
                Ok(Some(GenOutOrC::C(c)))
            }
        }
    }

    /// Evaluate `yield` / `yield*` / `await` (inside a generator or async function).
    pub fn eval_yield(&mut self, arg: Option<&Expr>, delegate: bool, env: &Env, pos: u32) -> R<V> {
        if self.ystack.is_empty() {
            let what = if delegate { "yield*" } else { "yield" };
            return self.throw("SyntaxError", format!("'{}' is only valid in generator functions", what), Some(pos));
        }
        if !delegate {
            let inner = arg.is_some_and(expr_has_yield);
            let early = if inner {
                match arg {
                    Some(a) => Some(self.eval(a, env)?),
                    None => None,
                }
            } else {
                None
            };
            let ctx = self.ystack.last_mut().unwrap();
            if ctx.cursor < ctx.memo.len() {
                let r = ctx.memo[ctx.cursor].clone();
                ctx.cursor += 1;
                return resume_value(r);
            }
            let v = match early {
                Some(v) => v,
                None => match arg {
                    Some(a) => self.eval(a, env)?,
                    None => self.undef(),
                },
            };
            return Err(Ctrl::Yield(v));
        }
        let (idx, replay) = {
            let ctx = self.ystack.last_mut().unwrap();
            let idx = ctx.cursor;
            if idx < ctx.memo.len() {
                ctx.cursor += 1;
                (idx, Some(ctx.memo[idx].clone()))
            } else {
                (idx, None)
            }
        };
        if let Some(r) = replay {
            return resume_value(r);
        }
        let active = {
            let ctx = self.ystack.last_mut().unwrap();
            match ctx.delegate.take() {
                Some((p, it)) if p == idx => Some((it, ctx.pending.take().unwrap_or(Resume::Next(V::Undef)))),
                other => {
                    ctx.delegate = other;
                    None
                }
            }
        };
        let (iter, sent) = match active {
            Some(x) => x,
            None => {
                let v = match arg {
                    Some(a) => self.eval(a, env)?,
                    None => self.undef(),
                };
                let it = match self.lazy_iterator(&v)? {
                    Some(i) => i,
                    None => {
                        let items = self.iterate(&v, Some(pos))?;
                        crate::collections::list_iterator(items)
                    }
                };
                (it, Resume::Next(V::Undef))
            }
        };
        let (val, done) = self.iter_resume(&iter, sent)?;
        let ctx = self.ystack.last_mut().unwrap();
        if done {
            if ctx.memo.len() == idx {
                ctx.memo.push(Resume::Next(val.clone()));
            }
            ctx.cursor = idx + 1;
            Ok(val)
        } else {
            ctx.delegate = Some((idx, iter));
            Err(Ctrl::Yield(val))
        }
    }

    /// An iterator for values iterated lazily (generators, iterator protocol objects), or
    /// `None` for values that are simply materialized.
    pub fn lazy_iterator(&mut self, v: &V) -> R<Option<V>> {
        let V::Obj(o) = v else { return Ok(None) };
        let kind = o.borrow().kind.clone();
        match kind {
            ObjKind::Generator(_) => return Ok(Some(v.clone())),
            ObjKind::Coll(c) => {
                if matches!(&*c.0.borrow(), crate::collections::Coll::Iter(..)) {
                    return Ok(Some(v.clone()));
                }
                return Ok(None);
            }
            ObjKind::Plain => {}
            _ => return Ok(None),
        }
        match self.d {
            Dialect::Js => {
                let key = self.well_known_key("iterator");
                let f = o.borrow().lookup(&key);
                if let Some(f) = f {
                    if f.is_callable() {
                        let it = self.call(&f, v.clone(), vec![], 0)?;
                        return Ok(Some(it));
                    }
                }
                Ok(None)
            }
            Dialect::Python => {
                let f = o.borrow().lookup("__iter__");
                if let Some(f) = f {
                    if f.is_callable() {
                        let it = self.call_method(v, "__iter__", vec![], 0)?;
                        return Ok(Some(it));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// Next value of a lazy iterator (`None` when exhausted).
    pub fn iter_next(&mut self, it: &V) -> R<Option<V>> {
        let (v, done) = self.iter_resume(it, Resume::Next(V::Undef))?;
        Ok(if done { None } else { Some(v) })
    }

    /// Resume a lazy iterator: `(value, done)`.
    pub fn iter_resume(&mut self, it: &V, r: Resume) -> R<(V, bool)> {
        if let V::Obj(o) = it {
            let kind = o.borrow().kind.clone();
            match kind {
                ObjKind::Generator(g) => return self.gen_resume(&g, r),
                ObjKind::Coll(c) => {
                    if let crate::collections::Coll::Iter(items, i) = &mut *c.0.borrow_mut() {
                        if let Resume::Throw(e) = r {
                            return Err(Ctrl::Throw(e));
                        }
                        let x = items.get(*i).cloned();
                        *i += 1;
                        return Ok(match x {
                            Some(v) => (v, false),
                            None => (V::Undef, true),
                        });
                    }
                }
                _ => {}
            }
        }
        if self.d == Dialect::Python {
            return match self.call_method(it, "__next__", vec![], 0) {
                Ok(v) => Ok((v, false)),
                Err(Ctrl::Throw(e)) if crate::builtins::error_name(&e) == "StopIteration" => Ok((V::Null, true)),
                Err(e) => Err(e),
            };
        }
        let res = match r {
            Resume::Next(v) => self.call_method(it, "next", vec![v], 0)?,
            Resume::Throw(e) => {
                let has = matches!(it, V::Obj(o) if o.borrow().lookup("throw").is_some());
                if !has {
                    return Err(Ctrl::Throw(e));
                }
                self.call_method(it, "throw", vec![e], 0)?
            }
            Resume::Return(v) => {
                let has = matches!(it, V::Obj(o) if o.borrow().lookup("return").is_some());
                if !has {
                    return Ok((v, true));
                }
                self.call_method(it, "return", vec![v], 0)?
            }
        };
        let done = self.get_member(&res, "done", 0)?;
        let value = self.get_member(&res, "value", 0)?;
        Ok((value, self.truthy(&done)))
    }

    /// `{value, done}` iterator result object.
    pub fn iter_result(&self, v: V, done: bool) -> V {
        let mut p = PropMap::default();
        p.set("value", v);
        p.set("done", V::Bool(done));
        V::obj(p)
    }

    /// Methods of generator objects.
    pub fn generator_method(&mut self, g: &Internal<GenState>, recv: &V, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
        let a0 = args.first().cloned().unwrap_or(V::Undef);
        let py = self.d == Dialect::Python;
        Ok(Some(match (name, py) {
            ("next", false) => {
                let (v, done) = self.gen_resume(g, Resume::Next(a0))?;
                self.iter_result(v, done)
            }
            ("return", false) => {
                let (v, done) = self.gen_resume(g, Resume::Return(a0))?;
                self.iter_result(v, done)
            }
            ("throw", false) => {
                let (v, done) = self.gen_resume(g, Resume::Throw(a0))?;
                self.iter_result(v, done)
            }
            ("__next__", true) | ("send", true) => {
                let sent = if name == "send" { a0 } else { V::Null };
                let (v, done) = self.gen_resume(g, Resume::Next(sent))?;
                if done {
                    let e = self.make_error("StopIteration", String::new(), Some(pos));
                    if let V::Obj(o) = &e {
                        o.borrow_mut().props.set("value", v);
                    }
                    return Err(Ctrl::Throw(e));
                }
                v
            }
            ("throw", true) => {
                let e = match &a0 {
                    V::Host(_) => self.call(&a0, V::Undef, args[1..].to_vec(), pos)?,
                    _ => a0,
                };
                let (v, done) = self.gen_resume(g, Resume::Throw(e))?;
                if done {
                    return self.throw("StopIteration", String::new(), Some(pos));
                }
                v
            }
            ("close", true) => {
                self.gen_resume(g, Resume::Return(V::Null))?;
                V::Null
            }
            ("__iter__", true) | ("%self", _) => recv.clone(),
            ("toString", false) => V::str("[object Generator]"),
            _ => return Ok(None),
        }))
    }
}

fn set_phase(st: &mut GenState, p: Phase) {
    if let Some(Frame::Loop { phase, .. }) = st.frames.last_mut() {
        *phase = p;
    }
}

fn bump_switch(st: &mut GenState, s: &Stmt) {
    for f in st.frames.iter_mut().rev() {
        if let Frame::Switch { stmt, case, idx, .. } = f {
            if let Stmt::Switch(_, cases) = stmt.get() {
                if cases.get(*case).and_then(|c| c.1.get(*idx)).is_some_and(|x| std::ptr::eq(x, s)) {
                    *idx += 1;
                    return;
                }
            }
        }
    }
}

enum GenOutOrC {
    Out(GenOut),
    C(Completion),
}

fn resume_value(r: Resume) -> R<V> {
    match r {
        Resume::Next(v) => Ok(v),
        Resume::Throw(e) => Err(Ctrl::Throw(e)),
        Resume::Return(v) => Err(Ctrl::Return(v)),
    }
}
