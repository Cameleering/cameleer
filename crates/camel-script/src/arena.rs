//! Cycle-safe teardown of the runtime object graph.
//!
//! Script values are reference counted (`Rc`), so self-referencing structures (`o.self = o`,
//! closures stored in the objects they capture, prototypes pointing back to their constructor,
//! suspended generators holding their own scope...) would never be freed. Every container
//! allocated during an evaluation is registered here (weakly); when the evaluation ends, all
//! of them are emptied, which breaks every cycle. The results of an evaluation are converted to
//! `camel_core::Value` before that, so nothing observable is lost.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use crate::value::{ArrRef, Closure, Obj, ObjKind, ObjRef, V};

/// Containers allocated during one evaluation (weakly referenced).
#[derive(Default)]
pub struct Arena {
    objs: Vec<Weak<RefCell<Obj>>>,
    arrs: Vec<Weak<RefCell<Vec<V>>>>,
    funcs: Vec<Weak<Closure>>,
    next_prune: usize,
}

thread_local! {
    static ARENA: RefCell<Option<Arena>> = const { RefCell::new(None) };
    static SURVIVORS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Containers of the last evaluation on this thread still alive after its teardown (0 unless
/// something leaked; diagnostics / tests).
pub fn last_survivors() -> usize {
    SURVIVORS.with(|s| s.get())
}

fn with_arena(f: impl FnOnce(&mut Arena)) {
    ARENA.with(|a| {
        if let Ok(mut a) = a.try_borrow_mut() {
            if let Some(a) = a.as_mut() {
                f(a);
                let n = a.objs.len() + a.arrs.len() + a.funcs.len();
                if n > a.next_prune.max(4096) {
                    a.objs.retain(|w| w.strong_count() > 0);
                    a.arrs.retain(|w| w.strong_count() > 0);
                    a.funcs.retain(|w| w.strong_count() > 0);
                    a.next_prune = 2 * (a.objs.len() + a.arrs.len() + a.funcs.len());
                }
            }
        }
    });
}

/// A new tracked object.
pub fn new_obj_ref(o: Obj) -> ObjRef {
    let r = Rc::new(RefCell::new(o));
    with_arena(|a| a.objs.push(Rc::downgrade(&r)));
    r
}

/// A new tracked array.
pub fn new_arr_ref(v: Vec<V>) -> ArrRef {
    let r = Rc::new(RefCell::new(v));
    with_arena(|a| a.arrs.push(Rc::downgrade(&r)));
    r
}

/// A new tracked closure.
pub fn new_closure(c: Closure) -> Rc<Closure> {
    let r = Rc::new(c);
    with_arena(|a| a.funcs.push(Rc::downgrade(&r)));
    r
}

/// Install a fresh arena on this thread; returns the previous one (nested evaluations).
pub fn enter() -> Option<Arena> {
    ARENA.with(|a| a.borrow_mut().replace(Arena::default()))
}

/// Move the current arena out (to hand it to another stack segment).
pub fn take() -> Option<Arena> {
    ARENA.with(|a| a.borrow_mut().take())
}

/// Install an arena taken with [`take`].
pub fn put(arena: Option<Arena>) {
    ARENA.with(|a| *a.borrow_mut() = arena);
}

/// Empty every container of the current arena (breaking cycles) and restore `prev`.
pub fn exit(prev: Option<Arena>) {
    let cur = ARENA.with(|a| std::mem::replace(&mut *a.borrow_mut(), prev));
    let Some(cur) = cur else { return };
    // take the contents first and drop them afterwards: every drop is then shallow
    let mut props = Vec::new();
    let mut protos = Vec::new();
    let mut kinds = Vec::new();
    let mut items: Vec<Vec<V>> = Vec::new();
    let mut homes = Vec::new();
    for w in &cur.objs {
        if let Some(o) = w.upgrade() {
            if let Ok(mut b) = o.try_borrow_mut() {
                props.push(std::mem::take(&mut b.props));
                protos.push(b.proto.take());
                kinds.push(std::mem::replace(&mut b.kind, ObjKind::Plain));
            }
        }
    }
    for w in &cur.arrs {
        if let Some(a) = w.upgrade() {
            if let Ok(mut b) = a.try_borrow_mut() {
                items.push(std::mem::take(&mut *b));
            }
        }
    }
    for w in &cur.funcs {
        if let Some(c) = w.upgrade() {
            if let Ok(mut p) = c.props.try_borrow_mut() {
                props.push(std::mem::take(&mut *p));
            }
            if let Ok(mut h) = c.home.try_borrow_mut() {
                homes.push(h.take());
            }
            if let Ok(mut vars) = c.env.vars.try_borrow_mut() {
                items.push(vars.drain().map(|(_, v)| v).collect());
            }
        }
    }
    drop(kinds);
    drop(props);
    drop(protos);
    drop(items);
    drop(homes);
    let alive = cur.objs.iter().filter(|w| w.strong_count() > 0).count() + cur.arrs.iter().filter(|w| w.strong_count() > 0).count() + cur.funcs.iter().filter(|w| w.strong_count() > 0).count();
    SURVIVORS.with(|s| s.set(alive));
}
