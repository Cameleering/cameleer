//! Promises, `async` / `await` and the run-to-completion job queues.
//!
//! Promise reactions run as microtasks; `setTimeout` callbacks are macrotasks ordered by their
//! (virtual) due time. Both queues are drained after the script body completes (and before its
//! result is returned), so `async` code runs to completion within one evaluation. An `async`
//! function is a generator ([`crate::gen`]) whose `await`s suspend it until the awaited promise
//! settles.

use std::collections::VecDeque;
use std::rc::Rc;

use crate::gen::{GenState, Resume};
use crate::interp::{Ctrl, Interp, R};
use crate::value::*;

/// Settlement state.
#[derive(Clone)]
pub enum PState {
    Pending,
    Fulfilled(V),
    Rejected(V),
}

/// A reaction registered with `then` / `await`.
#[derive(Clone)]
pub struct Reaction {
    pub on_ful: V,
    pub on_rej: V,
    /// The derived promise resolved with the handler's result.
    pub target: Option<V>,
    /// `await`: the suspended async function to resume.
    pub resume: Option<Internal<GenState>>,
    /// `finally` handler semantics.
    pub finally: bool,
}

/// The state of a promise.
pub struct PromiseState {
    pub state: PState,
    pub reactions: Vec<Reaction>,
    /// Resolve / reject already called (the promise may still be pending, adopting another).
    pub resolved: bool,
}

/// A queued job.
pub enum Job {
    React { r: Reaction, ok: bool, value: V },
    Thenable { promise: V, thenable: V, then: V },
    Call(V, Vec<V>),
}

/// A pending timer (macrotask).
pub struct Timer {
    pub due: f64,
    pub seq: u64,
    pub id: i64,
    pub f: V,
    pub args: Vec<V>,
    pub interval: Option<f64>,
}

/// Job queues of an interpreter.
#[derive(Default)]
pub struct Queues {
    pub micro: VecDeque<Job>,
    pub timers: Vec<Timer>,
    pub now: f64,
    pub seq: u64,
    pub next_id: i64,
}

fn pstate(v: &V) -> Option<Internal<PromiseState>> {
    match v {
        V::Obj(o) => match &o.borrow().kind {
            ObjKind::Promise(p) => Some(p.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Whether a value is a promise.
pub fn is_promise(v: &V) -> bool {
    pstate(v).is_some()
}

/// The settled state of a promise value.
pub fn settled(v: &V) -> Option<PState> {
    pstate(v).map(|p| p.0.borrow().state.clone())
}

impl Interp {
    /// A new pending promise.
    pub fn new_promise(&mut self) -> V {
        let st = PromiseState { state: PState::Pending, reactions: vec![], resolved: false };
        V::Obj(new_obj_ref(Obj { props: PropMap::default(), proto: None, kind: ObjKind::Promise(Internal::new(st)) }))
    }

    fn resolving_fn(&self, p: &V, which: &str) -> V {
        V::Method(Rc::new((p.clone(), which.to_string())))
    }

    /// `Promise.resolve(v)`: `v` itself when it is a promise.
    pub fn promise_resolve(&mut self, v: V) -> R<V> {
        if is_promise(&v) {
            return Ok(v);
        }
        let p = self.new_promise();
        self.resolve_promise(&p, v)?;
        Ok(p)
    }

    /// Resolve a promise with a value (adopting thenables).
    pub fn resolve_promise(&mut self, p: &V, v: V) -> R<()> {
        let Some(st) = pstate(p) else { return Ok(()) };
        if v.same(p) {
            let e = self.make_error("TypeError", "Chaining cycle detected for promise".into(), None);
            self.settle(&st, false, e);
            return Ok(());
        }
        if let V::Obj(o) = &v {
            let then = if is_promise(&v) { Some(V::Undef) } else { o.borrow().lookup("then").filter(|f| f.is_callable()) };
            if let Some(then) = then {
                self.queues.micro.push_back(Job::Thenable { promise: p.clone(), thenable: v.clone(), then });
                return Ok(());
            }
        }
        self.settle(&st, true, v);
        Ok(())
    }

    /// Reject a promise.
    pub fn reject_promise(&mut self, p: &V, e: V) {
        if let Some(st) = pstate(p) {
            self.settle(&st, false, e);
        }
    }

    fn settle(&mut self, st: &Internal<PromiseState>, ok: bool, v: V) {
        let reactions = {
            let mut b = st.0.borrow_mut();
            if !matches!(b.state, PState::Pending) {
                return;
            }
            b.state = if ok { PState::Fulfilled(v.clone()) } else { PState::Rejected(v.clone()) };
            std::mem::take(&mut b.reactions)
        };
        for r in reactions {
            self.queues.micro.push_back(Job::React { r, ok, value: v.clone() });
        }
    }

    /// Register a reaction (queued immediately when already settled).
    pub fn add_reaction(&mut self, p: &V, r: Reaction) {
        let Some(st) = pstate(p) else { return };
        let state = st.0.borrow().state.clone();
        match state {
            PState::Pending => st.0.borrow_mut().reactions.push(r),
            PState::Fulfilled(v) => self.queues.micro.push_back(Job::React { r, ok: true, value: v }),
            PState::Rejected(v) => self.queues.micro.push_back(Job::React { r, ok: false, value: v }),
        }
    }

    /// `p.then(onFulfilled, onRejected)`.
    pub fn promise_then(&mut self, p: &V, on_ful: V, on_rej: V, finally: bool) -> V {
        let target = self.new_promise();
        self.add_reaction(p, Reaction { on_ful, on_rej, target: Some(target.clone()), resume: None, finally });
        target
    }

    fn run_job(&mut self, job: Job) -> R<()> {
        match job {
            Job::Call(f, args) => {
                // errors of queued callbacks surface as uncaught exceptions
                self.call(&f, V::Undef, args, 0)?;
                Ok(())
            }
            Job::Thenable { promise, thenable, then } => {
                if is_promise(&thenable) {
                    // adopt the state of a native promise
                    let r = Reaction { on_ful: self.resolving_fn(&promise, "%adopt_ok"), on_rej: self.resolving_fn(&promise, "%adopt_err"), target: None, resume: None, finally: false };
                    self.add_reaction(&thenable, r);
                    return Ok(());
                }
                let res = self.resolving_fn(&promise, "%adopt_ok");
                let rej = self.resolving_fn(&promise, "%adopt_err");
                if let Err(Ctrl::Throw(e)) = self.call(&then, thenable, vec![res, rej], 0) {
                    self.reject_promise(&promise, e);
                }
                Ok(())
            }
            Job::React { r, ok, value } => {
                if let Some(g) = r.resume {
                    let resume = if ok { Resume::Next(value) } else { Resume::Throw(value) };
                    return self.async_step(&g, resume);
                }
                let handler = if r.finally {
                    r.on_ful.clone()
                } else if ok {
                    r.on_ful.clone()
                } else {
                    r.on_rej.clone()
                };
                if r.finally {
                    if handler.is_callable() {
                        if let Err(Ctrl::Throw(e)) = self.call(&handler, V::Undef, vec![], 0) {
                            if let Some(t) = &r.target {
                                self.reject_promise(t, e);
                            }
                            return Ok(());
                        }
                    }
                    if let Some(t) = &r.target {
                        if ok {
                            self.resolve_promise(t, value)?;
                        } else {
                            self.reject_promise(t, value);
                        }
                    }
                    return Ok(());
                }
                if !handler.is_callable() {
                    if let Some(t) = &r.target {
                        if ok {
                            self.resolve_promise(t, value)?;
                        } else {
                            self.reject_promise(t, value);
                        }
                    }
                    return Ok(());
                }
                match self.call(&handler, V::Undef, vec![value], 0) {
                    Ok(x) => {
                        if let Some(t) = &r.target {
                            self.resolve_promise(t, x)?;
                        }
                    }
                    Err(Ctrl::Throw(e)) => {
                        if let Some(t) = &r.target {
                            self.reject_promise(t, e);
                        }
                    }
                    Err(other) => return Err(other),
                }
                Ok(())
            }
        }
    }

    /// Start or continue an async function.
    pub fn async_step(&mut self, g: &Internal<GenState>, r: Resume) -> R<()> {
        let promise = g.0.borrow().promise.clone().unwrap_or(V::Undef);
        match self.gen_resume(g, r) {
            Ok((v, true)) => self.resolve_promise(&promise, v),
            Ok((v, false)) => {
                // await v
                let p = self.promise_resolve(v)?;
                self.add_reaction(&p, Reaction { on_ful: V::Undef, on_rej: V::Undef, target: None, resume: Some(g.clone()), finally: false });
                Ok(())
            }
            Err(Ctrl::Throw(e)) => {
                self.reject_promise(&promise, e);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Call an async function whose generator state is ready: returns its promise.
    pub fn start_async(&mut self, g: Internal<GenState>) -> R<V> {
        let p = self.new_promise();
        g.0.borrow_mut().promise = Some(p.clone());
        self.async_step(&g, Resume::Next(V::Undef))?;
        Ok(p)
    }

    /// Run all queued microtasks and timers.
    pub fn drain_jobs(&mut self) -> R<()> {
        loop {
            while let Some(job) = self.queues.micro.pop_front() {
                self.step_pub()?;
                self.run_job(job)?;
            }
            if self.queues.timers.is_empty() {
                return Ok(());
            }
            let mut best = 0;
            for (i, t) in self.queues.timers.iter().enumerate() {
                let b = &self.queues.timers[best];
                if t.due < b.due || (t.due == b.due && t.seq < b.seq) {
                    best = i;
                }
            }
            let t = self.queues.timers.remove(best);
            self.queues.now = self.queues.now.max(t.due);
            if let Some(iv) = t.interval {
                self.queues.seq += 1;
                self.queues.timers.push(Timer { due: self.queues.now + iv.max(1.0), seq: self.queues.seq, id: t.id, f: t.f.clone(), args: t.args.clone(), interval: t.interval });
            }
            self.step_pub()?;
            self.call(&t.f, V::Undef, t.args, 0)?;
        }
    }

    /// `setTimeout` / `setInterval`: returns the timer id.
    pub fn add_timer(&mut self, f: V, delay: f64, args: Vec<V>, interval: bool) -> i64 {
        self.queues.seq += 1;
        self.queues.next_id += 1;
        let d = if delay.is_nan() { 0.0 } else { delay.max(0.0) };
        let id = self.queues.next_id;
        self.queues.timers.push(Timer { due: self.queues.now + d, seq: self.queues.seq, id, f, args, interval: if interval { Some(d) } else { None } });
        id
    }

    /// `clearTimeout` / `clearInterval`.
    pub fn clear_timer(&mut self, id: i64) {
        self.queues.timers.retain(|t| t.id != id);
    }

    /// Methods of promise objects.
    pub fn promise_method(&mut self, recv: &V, name: &str, args: &[V]) -> R<Option<V>> {
        let a0 = args.first().cloned().unwrap_or(V::Undef);
        let a1 = args.get(1).cloned().unwrap_or(V::Undef);
        Ok(Some(match name {
            "then" => self.promise_then(recv, a0, a1, false),
            "catch" => self.promise_then(recv, V::Undef, a0, false),
            "finally" => self.promise_then(recv, a0, V::Undef, true),
            "toString" => V::str("[object Promise]"),
            _ => return Ok(None),
        }))
    }

    /// Internal function values (`%resolve`, `%reject`, combinator callbacks).
    pub fn promise_internal(&mut self, recv: &V, name: &str, args: &[V]) -> R<Option<V>> {
        let a0 = args.first().cloned().unwrap_or(V::Undef);
        match name {
            "%adopt_ok" => {
                self.resolve_promise(recv, a0)?;
                Ok(Some(V::Undef))
            }
            "%adopt_err" => {
                self.reject_promise(recv, a0);
                Ok(Some(V::Undef))
            }
            "%resolve" | "%reject" => {
                let Some(st) = pstate(recv) else { return Ok(Some(V::Undef)) };
                if st.0.borrow().resolved {
                    return Ok(Some(V::Undef));
                }
                st.0.borrow_mut().resolved = true;
                if name == "%resolve" {
                    self.resolve_promise(recv, a0)?;
                } else {
                    self.reject_promise(recv, a0);
                }
                Ok(Some(V::Undef))
            }
            _ if name.starts_with("%comb:") => {
                // %comb:<kind>:<index> on an aggregate object {result, values, remaining}
                let mut parts = name[6..].splitn(2, ':');
                let kind = parts.next().unwrap_or("").to_string();
                let idx: usize = parts.next().and_then(|i| i.parse().ok()).unwrap_or(0);
                let V::Obj(agg) = recv else { return Ok(Some(V::Undef)) };
                let (result, values, remaining) = {
                    let b = agg.borrow();
                    (b.props.get("result").cloned().unwrap_or(V::Undef), b.props.get("values").cloned().unwrap_or(V::Undef), b.props.get("remaining").and_then(|v| v.as_f64()).unwrap_or(0.0) as i64)
                };
                let set_slot = |v: V| {
                    if let V::Arr(a) = &values {
                        if let Some(s) = a.borrow_mut().get_mut(idx) {
                            *s = v;
                        }
                    }
                };
                match kind.as_str() {
                    "all" | "any_rej" => {
                        set_slot(a0);
                        let left = remaining - 1;
                        agg.borrow_mut().props.set("remaining", V::Int(left));
                        if left == 0 {
                            if kind == "all" {
                                self.resolve_promise(&result, values)?;
                            } else {
                                let e = self.make_error("AggregateError", "All promises were rejected".into(), None);
                                if let V::Obj(o) = &e {
                                    o.borrow_mut().props.set("errors", values);
                                }
                                self.reject_promise(&result, e);
                            }
                        }
                    }
                    "settled_ok" | "settled_err" => {
                        let mut p = PropMap::default();
                        if kind == "settled_ok" {
                            p.set("status", V::str("fulfilled"));
                            p.set("value", a0);
                        } else {
                            p.set("status", V::str("rejected"));
                            p.set("reason", a0);
                        }
                        set_slot(V::obj(p));
                        let left = remaining - 1;
                        agg.borrow_mut().props.set("remaining", V::Int(left));
                        if left == 0 {
                            self.resolve_promise(&result, values)?;
                        }
                    }
                    _ => {}
                }
                Ok(Some(V::Undef))
            }
            _ => Ok(None),
        }
    }

    /// `Promise.*` static functions.
    pub fn promise_static(&mut self, name: &str, args: Vec<V>, pos: u32) -> R<V> {
        let a0 = args.first().cloned().unwrap_or(V::Undef);
        match name {
            "resolve" => self.promise_resolve(a0),
            "reject" => {
                let p = self.new_promise();
                self.reject_promise(&p, a0);
                Ok(p)
            }
            "all" | "allSettled" | "any" | "race" => {
                let items = self.iterate(&a0, Some(pos))?;
                let result = self.new_promise();
                if name == "race" {
                    for it in items {
                        let p = self.promise_resolve(it)?;
                        let res = self.resolving_fn(&result, "%resolve");
                        let rej = self.resolving_fn(&result, "%reject");
                        self.add_reaction(&p, Reaction { on_ful: res, on_rej: rej, target: None, resume: None, finally: false });
                    }
                    return Ok(result);
                }
                let n = items.len();
                if n == 0 {
                    if name == "any" {
                        let e = self.make_error("AggregateError", "All promises were rejected".into(), None);
                        self.reject_promise(&result, e);
                    } else {
                        self.resolve_promise(&result, V::arr(vec![]))?;
                    }
                    return Ok(result);
                }
                let mut ap = PropMap::default();
                ap.set("result", result.clone());
                ap.set("values", V::arr(vec![V::Undef; n]));
                ap.set("remaining", V::Int(n as i64));
                let agg = V::obj(ap);
                for (i, it) in items.into_iter().enumerate() {
                    let p = self.promise_resolve(it)?;
                    let (ful, rej) = match name {
                        "all" => (V::Method(Rc::new((agg.clone(), format!("%comb:all:{}", i)))), self.resolving_fn(&result, "%reject")),
                        "allSettled" => (V::Method(Rc::new((agg.clone(), format!("%comb:settled_ok:{}", i)))), V::Method(Rc::new((agg.clone(), format!("%comb:settled_err:{}", i))))),
                        _ => (self.resolving_fn(&result, "%resolve"), V::Method(Rc::new((agg.clone(), format!("%comb:any_rej:{}", i))))),
                    };
                    self.add_reaction(&p, Reaction { on_ful: ful, on_rej: rej, target: None, resume: None, finally: false });
                }
                Ok(result)
            }
            "withResolvers" => {
                let p = self.new_promise();
                let mut o = PropMap::default();
                o.set("promise", p.clone());
                o.set("resolve", self.resolving_fn(&p, "%resolve"));
                o.set("reject", self.resolving_fn(&p, "%reject"));
                Ok(V::obj(o))
            }
            _ => self.type_error(format!("Promise.{} is not a function", name), Some(pos)),
        }
    }

    /// `new Promise(executor)`.
    pub fn construct_promise(&mut self, args: &[V], pos: u32) -> R<V> {
        let ex = args.first().cloned().unwrap_or(V::Undef);
        if !ex.is_callable() {
            return self.type_error("Promise resolver is not a function".into(), Some(pos));
        }
        let p = self.new_promise();
        let res = self.resolving_fn(&p, "%resolve");
        let rej = self.resolving_fn(&p, "%reject");
        if let Err(Ctrl::Throw(e)) = self.call(&ex, V::Undef, vec![res, rej.clone()], pos) {
            self.promise_internal(&p, "%reject", &[e])?;
        }
        Ok(p)
    }
}
