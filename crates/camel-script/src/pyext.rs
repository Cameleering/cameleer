//! Python runtime extensions: `import` of user modules (files relative to the importing module /
//! script, the working directory or the classpath) and of the bundled modules (`collections`,
//! `functools`, `itertools`, `contextlib`, `dataclasses`, `asyncio`, `typing`, `abc`,
//! `operator`, `copy`, `sys`, `string`, `json`, `math`, `re`), coroutines with an event loop,
//! metaclasses, `type(name, bases, ns)`, `match` statement helpers and frozen dataclasses.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use crate::ast::{ClassDef, ClassStyle, Dialect, FuncBody, FuncDef, Program};
use crate::interp::{Ctrl, Env, Interp, R};
use crate::value::*;

/// Native (Rust implemented) modules.
const NATIVE: &[&str] = &["json", "math", "re", "_collections", "dataclasses", "asyncio", "typing", "abc", "operator", "copy", "sys", "string", "time", "random"];

/// Kinds of dict subclasses (`collections`), kept by address in [`Interp::py_dicts`].
#[derive(Clone)]
pub enum PyDict {
    Default(V),
    Counter,
    Ordered,
}

/// Install the Python globals of this module.
pub fn install(it: &mut Interp) {
    for n in ["%pyimport", "%pyfrom", "%metaclass", "%match_seq", "%match_map", "%match_rest", "%match_pos", "%asyncIter"] {
        let internal = if n == "%asyncIter" { "%async_iter".to_string() } else { n.to_string() };
        it.globals.define(n, V::Method(Rc::new((V::Null, internal))));
    }
}

// ====================================================================== bundled source modules

const ITERTOOLS: &str = r#"
def count(start=0, step=1):
    n = start
    while True:
        yield n
        n += step

def cycle(iterable):
    saved = list(iterable)
    while saved:
        for x in saved:
            yield x

def repeat(obj, times=None):
    if times is None:
        while True:
            yield obj
    else:
        for _ in range(times):
            yield obj

def islice(iterable, *args):
    if len(args) == 1:
        start, stop, step = 0, args[0], 1
    else:
        start = args[0] or 0
        stop = args[1]
        step = args[2] if len(args) > 2 and args[2] is not None else 1
    if stop is not None and start >= stop:
        return
    i = 0
    nxt = start
    for x in iterable:
        if i == nxt:
            yield x
            nxt += step
            if stop is not None and nxt >= stop:
                return
        i += 1

def chain(*iterables):
    for it in iterables:
        for x in it:
            yield x

def _from_iterable(iterables):
    for it in iterables:
        for x in it:
            yield x

chain.from_iterable = _from_iterable

def accumulate(iterable, func=None, initial=None):
    total = initial
    first = initial is None
    if not first:
        yield total
    for x in iterable:
        if first:
            total = x
            first = False
        elif func is None:
            total = total + x
        else:
            total = func(total, x)
        yield total

def takewhile(pred, iterable):
    for x in iterable:
        if not pred(x):
            return
        yield x

def dropwhile(pred, iterable):
    dropping = True
    for x in iterable:
        if dropping and pred(x):
            continue
        dropping = False
        yield x

def filterfalse(pred, iterable):
    for x in iterable:
        if not (pred(x) if pred is not None else x):
            yield x

def compress(data, selectors):
    return [d for d, s in zip(data, selectors) if s]

def starmap(func, iterable):
    for args in iterable:
        yield func(*args)

def zip_longest(*iterables, fillvalue=None):
    lists = [list(x) for x in iterables]
    n = max([len(l) for l in lists]) if lists else 0
    return [tuple([l[i] if i < len(l) else fillvalue for l in lists]) for i in range(n)]

def pairwise(iterable):
    items = list(iterable)
    return [(items[i], items[i + 1]) for i in range(len(items) - 1)]

def batched(iterable, n):
    items = list(iterable)
    return [tuple(items[i:i + n]) for i in range(0, len(items), n)]

def product(*iterables, repeat=1):
    pools = [list(p) for p in iterables] * repeat
    result = [[]]
    for pool in pools:
        result = [x + [y] for x in result for y in pool]
    return [tuple(r) for r in result]

def permutations(iterable, r=None):
    pool = list(iterable)
    n = len(pool)
    r = n if r is None else r
    if r > n:
        return []
    out = []
    def rec(prefix, rest):
        if len(prefix) == r:
            out.append(tuple(prefix))
            return
        for i in range(len(rest)):
            rec(prefix + [rest[i]], rest[:i] + rest[i + 1:])
    rec([], pool)
    return out

def combinations(iterable, r):
    pool = list(iterable)
    out = []
    def rec(start, prefix):
        if len(prefix) == r:
            out.append(tuple(prefix))
            return
        for i in range(start, len(pool)):
            rec(i + 1, prefix + [pool[i]])
    rec(0, [])
    return out

def combinations_with_replacement(iterable, r):
    pool = list(iterable)
    out = []
    def rec(start, prefix):
        if len(prefix) == r:
            out.append(tuple(prefix))
            return
        for i in range(start, len(pool)):
            rec(i, prefix + [pool[i]])
    rec(0, [])
    return out

def groupby(iterable, key=None):
    out = []
    cur = None
    group = None
    first = True
    for x in iterable:
        k = key(x) if key is not None else x
        if first or k != cur:
            if not first:
                out.append((cur, group))
            cur = k
            group = []
            first = False
        group.append(x)
    if not first:
        out.append((cur, group))
    return out

def tee(iterable, n=2):
    items = list(iterable)
    return tuple([list(items) for _ in range(n)])
"#;

const FUNCTOOLS: &str = r#"
def reduce(function, iterable, *initial):
    it = iter(iterable)
    if initial:
        value = initial[0]
    else:
        try:
            value = next(it)
        except StopIteration:
            raise TypeError('reduce() of empty iterable with no initial value')
    for element in it:
        value = function(value, element)
    return value

class partial:
    def __init__(self, func, *args, **keywords):
        self.func = func
        self.args = args
        self.keywords = keywords
    def __call__(self, *args, **keywords):
        kw = dict(self.keywords)
        kw.update(keywords)
        return self.func(*self.args, *args, **kw)

def _cached(f, maxsize=None):
    memo = {}
    stats = [0, 0]
    def wrapper(*args, **kw):
        key = repr([args, sorted(kw.items())])
        if key in memo:
            stats[0] += 1
            return memo[key]
        stats[1] += 1
        r = f(*args, **kw)
        memo[key] = r
        return r
    def cache_info():
        return {'hits': stats[0], 'misses': stats[1], 'maxsize': maxsize, 'currsize': len(memo)}
    def cache_clear():
        memo.clear()
        stats[0] = 0
        stats[1] = 0
    wrapper.cache_info = cache_info
    wrapper.cache_clear = cache_clear
    wrapper.__wrapped__ = f
    wrapper.__name__ = f.__name__
    return wrapper

def lru_cache(maxsize=128, typed=False):
    if callable(maxsize):
        return _cached(maxsize, 128)
    def deco(f):
        return _cached(f, maxsize)
    return deco

def cache(f):
    return _cached(f)

def wraps(wrapped, assigned=None, updated=None):
    def deco(f):
        f.__name__ = wrapped.__name__
        f.__doc__ = getattr(wrapped, '__doc__', None)
        f.__wrapped__ = wrapped
        return f
    return deco

def update_wrapper(wrapper, wrapped, assigned=None, updated=None):
    return wraps(wrapped)(wrapper)

def cmp_to_key(mycmp):
    class K:
        def __init__(self, obj):
            self.obj = obj
        def __lt__(self, other):
            return mycmp(self.obj, other.obj) < 0
        def __gt__(self, other):
            return mycmp(self.obj, other.obj) > 0
        def __eq__(self, other):
            return mycmp(self.obj, other.obj) == 0
        def __le__(self, other):
            return mycmp(self.obj, other.obj) <= 0
        def __ge__(self, other):
            return mycmp(self.obj, other.obj) >= 0
    return K

def total_ordering(cls):
    return cls

def cached_property(f):
    return property(f)

def singledispatch(func):
    registry = {}
    def dispatch(arg, *args, **kw):
        for t in registry:
            if isinstance(arg, t):
                return registry[t](arg, *args, **kw)
        return func(arg, *args, **kw)
    def register(t, f=None):
        if f is not None:
            registry[t] = f
            return f
        def deco(g):
            registry[t] = g
            return g
        return deco
    dispatch.register = register
    return dispatch
"#;

const CONTEXTLIB: &str = r#"
class _GeneratorContextManager:
    def __init__(self, gen):
        self.gen = gen
    def __enter__(self):
        try:
            return next(self.gen)
        except StopIteration:
            raise RuntimeError("generator didn't yield")
    def __exit__(self, typ, value, tb):
        if value is None:
            try:
                next(self.gen)
            except StopIteration:
                return False
            raise RuntimeError("generator didn't stop")
        try:
            self.gen.throw(value)
        except StopIteration:
            return True
        except BaseException as e:
            if e is value:
                return False
            raise
        return True

def contextmanager(func):
    def helper(*args, **kwds):
        return _GeneratorContextManager(func(*args, **kwds))
    return helper

class _AsyncGeneratorContextManager:
    def __init__(self, gen):
        self.gen = gen
    async def __aenter__(self):
        r = await self.gen.next()
        return r.value
    async def __aexit__(self, typ, value, tb):
        await self.gen.next()
        return False

def asynccontextmanager(func):
    def helper(*args, **kwds):
        return _AsyncGeneratorContextManager(func(*args, **kwds))
    return helper

class suppress:
    def __init__(self, *exceptions):
        self.exceptions = exceptions
    def __enter__(self):
        return None
    def __exit__(self, typ, value, tb):
        if value is None:
            return False
        for e in self.exceptions:
            if isinstance(value, e):
                return True
        return False

class closing:
    def __init__(self, thing):
        self.thing = thing
    def __enter__(self):
        return self.thing
    def __exit__(self, typ, value, tb):
        self.thing.close()
        return False

class nullcontext:
    def __init__(self, enter_result=None):
        self.enter_result = enter_result
    def __enter__(self):
        return self.enter_result
    def __exit__(self, typ, value, tb):
        return False

class ExitStack:
    def __init__(self):
        self._callbacks = []
    def __enter__(self):
        return self
    def enter_context(self, cm):
        r = cm.__enter__()
        self._callbacks.append(cm)
        return r
    def callback(self, fn, *args, **kw):
        self._callbacks.append(_Callback(fn, args, kw))
        return fn
    def close(self):
        self.__exit__(None, None, None)
    def __exit__(self, typ, value, tb):
        suppressed = False
        while self._callbacks:
            cm = self._callbacks.pop()
            if cm.__exit__(typ, value, tb):
                suppressed = True
                typ, value, tb = None, None, None
        return suppressed

class _Callback:
    def __init__(self, fn, args, kw):
        self.fn = fn
        self.args = args
        self.kw = kw
    def __exit__(self, typ, value, tb):
        self.fn(*self.args, **self.kw)
        return False
"#;

const COLLECTIONS: &str = r#"
import _collections as _n
Counter = _n.Counter
defaultdict = _n.defaultdict
OrderedDict = _n.OrderedDict

def namedtuple(typename, field_names, defaults=None, rename=False, module=None):
    if isinstance(field_names, str):
        field_names = field_names.replace(',', ' ').split()
    fields = tuple(field_names)
    return _n.namedtuple_type(typename, fields, defaults)

class deque:
    def __init__(self, iterable=(), maxlen=None):
        self.maxlen = maxlen
        self._items = []
        for x in iterable:
            self.append(x)
    def append(self, x):
        self._items.append(x)
        if self.maxlen is not None and len(self._items) > self.maxlen:
            self._items.pop(0)
    def appendleft(self, x):
        self._items.insert(0, x)
        if self.maxlen is not None and len(self._items) > self.maxlen:
            self._items.pop()
    def pop(self):
        if not self._items:
            raise IndexError('pop from an empty deque')
        return self._items.pop()
    def popleft(self):
        if not self._items:
            raise IndexError('pop from an empty deque')
        return self._items.pop(0)
    def extend(self, items):
        for x in items:
            self.append(x)
    def extendleft(self, items):
        for x in items:
            self.appendleft(x)
    def rotate(self, n=1):
        if self._items:
            n = n % len(self._items)
            self._items = self._items[-n:] + self._items[:-n]
    def clear(self):
        self._items = []
    def count(self, x):
        return self._items.count(x)
    def remove(self, x):
        self._items.remove(x)
    def reverse(self):
        self._items.reverse()
    def index(self, x):
        return self._items.index(x)
    def copy(self):
        return deque(self._items, self.maxlen)
    def __len__(self):
        return len(self._items)
    def __iter__(self):
        return iter(list(self._items))
    def __getitem__(self, i):
        return self._items[i]
    def __setitem__(self, i, v):
        self._items[i] = v
    def __contains__(self, x):
        return x in self._items
    def __bool__(self):
        return len(self._items) > 0
    def __eq__(self, other):
        return isinstance(other, deque) and self._items == other._items
    def __repr__(self):
        if self.maxlen is None:
            return 'deque(' + repr(self._items) + ')'
        return 'deque(' + repr(self._items) + ', maxlen=' + str(self.maxlen) + ')'

class ChainMap:
    def __init__(self, *maps):
        self.maps = list(maps) or [{}]
    def __getitem__(self, k):
        for m in self.maps:
            if k in m:
                return m[k]
        raise KeyError(k)
    def get(self, k, default=None):
        for m in self.maps:
            if k in m:
                return m[k]
        return default
    def __contains__(self, k):
        return any(k in m for m in self.maps)
    def __setitem__(self, k, v):
        self.maps[0][k] = v
    def keys(self):
        seen = []
        for m in self.maps:
            for k in m:
                if k not in seen:
                    seen.append(k)
        return seen
    def __iter__(self):
        return iter(self.keys())
    def __len__(self):
        return len(self.keys())
    def items(self):
        return [(k, self[k]) for k in self.keys()]
    def values(self):
        return [self[k] for k in self.keys()]
    def new_child(self, m=None):
        return ChainMap(m if m is not None else {}, *self.maps)
"#;

fn source_module(name: &str) -> Option<&'static Program> {
    static CELLS: OnceLock<Vec<(&'static str, OnceLock<Program>)>> = OnceLock::new();
    let cells = CELLS.get_or_init(|| ["itertools", "functools", "contextlib", "collections"].iter().map(|n| (*n, OnceLock::new())).collect());
    let (n, cell) = cells.iter().find(|(n, _)| *n == name)?;
    let src = match *n {
        "itertools" => ITERTOOLS,
        "functools" => FUNCTOOLS,
        "contextlib" => CONTEXTLIB,
        _ => COLLECTIONS,
    };
    Some(cell.get_or_init(|| crate::python::parse_program(src).expect("bundled Python module parses")))
}

// ====================================================================== imports

/// A module object: the public bindings of the module's scope.
fn module_object(env: &Env, name: &str) -> V {
    let mut p = PropMap::default();
    let vars = env.vars.borrow();
    let mut keys: Vec<&Rc<str>> = vars.keys().filter(|k| !k.starts_with('%')).collect();
    keys.sort();
    for k in keys {
        p.set(k, vars[k].clone());
    }
    p.set("__name__", V::str(name));
    V::obj(p)
}

impl Interp {
    /// `import name` (the top-level package when `top`).
    pub fn py_import(&mut self, name: &str, top: bool, pos: u32) -> R<V> {
        let m = self.py_load(name, pos)?;
        if top && name.contains('.') {
            let root = name.split('.').next().unwrap_or(name);
            if let Ok(r) = self.py_load(root, pos) {
                return Ok(r);
            }
            // namespace packages: nested objects down to the module
            let parts: Vec<&str> = name.split('.').collect();
            let mut cur = m;
            for p in parts[1..].iter().rev() {
                let mut o = PropMap::default();
                o.set(p, cur);
                cur = V::obj(o);
            }
            return Ok(cur);
        }
        Ok(m)
    }

    /// `from module import name`: an attribute of the module or a submodule.
    pub fn py_from(&mut self, module: &str, name: &str, pos: u32) -> R<V> {
        let sub = if module.ends_with('.') { format!("{}{}", module, name) } else { format!("{}.{}", module, name) };
        let m = match self.py_load(module, pos) {
            Ok(m) => m,
            // a namespace package (directory without __init__.py)
            Err(e) => return self.py_load(&sub, pos).map_err(|_| e),
        };
        let has = match &m {
            V::Obj(o) => o.borrow().props.contains(name),
            _ => true,
        };
        if has {
            return self.get_member(&m, name, pos);
        }
        match self.py_load(&sub, pos) {
            Ok(v) => Ok(v),
            Err(_) => self.throw("ImportError", format!("cannot import name '{}' from '{}'", name, module), Some(pos)),
        }
    }

    /// Load (once per evaluation) a module by its dotted name.
    pub fn py_load(&mut self, name: &str, pos: u32) -> R<V> {
        if let Some(n) = NATIVE.iter().find(|n| **n == name) {
            return Ok(V::host(Host::Ns(n)));
        }
        let key = format!("py:{}", name);
        if let Some(v) = self.imports.get(&key) {
            return Ok(v.clone());
        }
        if let Some(prog) = source_module(name) {
            let v = self.py_exec_module(prog, name, None, None)?;
            self.imports.insert(key, v.clone());
            return Ok(v);
        }
        let Some(file) = self.py_resolve(name) else {
            return self.throw("ModuleNotFoundError", format!("No module named '{}'", name.trim_start_matches('.')), Some(pos));
        };
        let fkey = format!("pyfile:{}", file.display());
        if let Some(v) = self.imports.get(&fkey) {
            return Ok(v.clone());
        }
        let src = match std::fs::read_to_string(&file) {
            Ok(s) => s,
            Err(e) => return self.throw("ImportError", format!("cannot read module '{}': {}", name, e), Some(pos)),
        };
        let prog = match crate::python::parse_program(&src) {
            Ok(p) => p,
            Err(e) => {
                let (l, c) = crate::error::line_col(&src, e.pos);
                return self.throw("SyntaxError", format!("{} (line {}, column {}): {}", file.display(), l, c, e.msg), Some(pos));
            }
        };
        let prog = Rc::new(prog);
        // the module's functions point into its AST: keep it alive for the evaluation
        self.import_asts.push(prog.clone());
        let dir = file.parent().map(|d| d.to_path_buf());
        let v = self.py_exec_module(&prog, name, dir, Some(src))?;
        self.imports.insert(fkey, v.clone());
        self.imports.insert(key, v.clone());
        // a submodule becomes an attribute of its (already loaded) package
        if let Some((pkg, last)) = name.rsplit_once('.') {
            if !pkg.is_empty() && !pkg.ends_with('.') {
                if let Ok(V::Obj(po)) = self.py_load(pkg, pos) {
                    po.borrow_mut().props.set(last, v.clone());
                }
            }
        }
        Ok(v)
    }

    fn py_exec_module(&mut self, prog: &Program, name: &str, dir: Option<std::path::PathBuf>, src: Option<String>) -> R<V> {
        let env = crate::interp::new_scope(Some(self.globals.clone()), true);
        env.define("__name__", V::str(name));
        self.track_env_pub(&env);
        let saved = src.map(|s| std::mem::replace(&mut self.src, Arc::from(s.as_str())));
        if let Some(d) = &dir {
            self.module_dirs.push(d.clone());
        }
        self.hoist_pub(&prog.body, &env);
        let mut r = Ok(());
        for s in &prog.body {
            match self.exec(s, &env) {
                Ok(_) => {}
                Err(Ctrl::Throw(e)) => {
                    r = Err(Ctrl::Throw(e));
                    break;
                }
                Err(_) => {}
            }
        }
        if dir.is_some() {
            self.module_dirs.pop();
        }
        if let Some(s) = saved {
            self.src = s;
        }
        r?;
        Ok(module_object(&env, name))
    }

    fn py_resolve(&self, name: &str) -> Option<std::path::PathBuf> {
        let dots = name.chars().take_while(|c| *c == '.').count();
        let rel = name[dots..].replace('.', "/");
        let candidates = |base: std::path::PathBuf| -> Option<std::path::PathBuf> {
            let f = if rel.is_empty() { base.join("__init__.py") } else { base.join(format!("{}.py", rel)) };
            if f.is_file() {
                return Some(f);
            }
            let pkg = base.join(&rel).join("__init__.py");
            pkg.is_file().then_some(pkg)
        };
        let here = self.module_dirs.last().cloned().or_else(|| self.script_dir.clone());
        if dots > 0 {
            let mut d = here.unwrap_or_else(|| std::path::PathBuf::from("."));
            for _ in 1..dots {
                d = d.parent().map(|p| p.to_path_buf()).unwrap_or(d);
            }
            return candidates(d);
        }
        if let Some(d) = here {
            if let Some(f) = candidates(d) {
                return Some(f);
            }
        }
        let mut dirs: Vec<std::path::PathBuf> = vec![".".into(), "src/main/resources".into(), "src/test/resources".into(), "src/main/python".into()];
        if let Ok(v) = std::env::var("PYTHONPATH") {
            dirs.extend(v.split(':').filter(|s| !s.is_empty()).map(std::path::PathBuf::from));
        }
        dirs.into_iter().find_map(candidates)
    }

    /// `from module import *`: define the module's public names in `env`.
    pub fn py_import_star(&mut self, module: &str, env: &Env, pos: u32) -> R<()> {
        let m = self.py_load(module, pos)?;
        match &m {
            V::Obj(o) => {
                let all = o.borrow().props.get("__all__").cloned();
                let names: Vec<String> = match all {
                    Some(a) => self.iterate(&a, Some(pos))?.iter().map(|x| self.display(x)).collect(),
                    None => o.borrow().props.keys().filter(|k| !k.starts_with('_')).map(|k| k.to_string()).collect(),
                };
                for n in names {
                    let v = self.get_member(&m, &n, pos)?;
                    env.define(&n, v);
                }
            }
            V::Host(h) => {
                if let Host::Ns(ns) = &**h {
                    for n in module_exports(ns) {
                        let v = self.get_member(&m, n, pos)?;
                        env.define(n, v);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn module_exports(ns: &str) -> &'static [&'static str] {
    match ns {
        "math" => &["pi", "e", "inf", "nan", "sqrt", "floor", "ceil", "pow", "log", "exp", "sin", "cos", "tan", "fabs", "gcd", "isclose", "factorial"],
        "json" => &["dumps", "loads"],
        "re" => &["match", "search", "findall", "sub", "split", "compile", "fullmatch", "finditer"],
        "dataclasses" => &["dataclass", "field", "asdict", "astuple", "replace", "fields", "is_dataclass"],
        "typing" => &["Any", "List", "Dict", "Optional", "Union", "Tuple", "Set", "Callable", "Iterable", "Iterator", "TypeVar", "Generic"],
        "asyncio" => &["run", "gather", "sleep", "create_task", "wait_for"],
        "operator" => &["itemgetter", "attrgetter", "add", "sub", "mul", "truediv", "neg", "not_"],
        "copy" => &["copy", "deepcopy"],
        _ => &[],
    }
}

// ====================================================================== native module members

impl Interp {
    /// Attribute values of native modules (constants and types).
    pub fn py_ns_member(&mut self, ns: &str, name: &str) -> Option<V> {
        Some(match (ns, name) {
            ("sys", "maxsize") => V::Int(i64::MAX),
            ("sys", "version") => V::str("3.12.0 (rust-camel)"),
            ("sys", "platform") => V::str(std::env::consts::OS),
            ("sys", "argv") => V::arr(vec![]),
            ("sys", "version_info") => V::arr(vec![V::Int(3), V::Int(12), V::Int(0)]),
            ("string", "ascii_letters") => V::str("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"),
            ("string", "ascii_lowercase") => V::str("abcdefghijklmnopqrstuvwxyz"),
            ("string", "ascii_uppercase") => V::str("ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
            ("string", "digits") => V::str("0123456789"),
            ("string", "hexdigits") => V::str("0123456789abcdefABCDEF"),
            ("string", "punctuation") => V::str("!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~"),
            ("string", "whitespace") => V::str(" \t\n\r\x0b\x0c"),
            ("typing", "TYPE_CHECKING") => V::Bool(false),
            ("typing", _) => V::host(Host::Type("object".into())),
            ("abc", "ABC") => V::host(Host::Type("object".into())),
            ("dataclasses", "MISSING") => V::Undef,
            _ => return None,
        })
    }

    /// Functions of native modules.
    pub fn py_call_ns_fn(&mut self, ns: &str, name: &str, args: Vec<V>, pos: u32) -> R<Option<V>> {
        let a0 = args.first().cloned().unwrap_or(V::Null);
        let a1 = args.get(1).cloned().unwrap_or(V::Null);
        Ok(Some(match (ns, name) {
            // ---------------- collections (native part)
            ("_collections", "Counter") => {
                let d = V::new_obj();
                self.py_register_dict(&d, PyDict::Counter);
                self.counter_update(&d, &args, 1, pos)?;
                d
            }
            ("_collections", "defaultdict") => {
                let d = V::new_obj();
                if let (V::Obj(o), Some(V::Obj(init))) = (&d, args.get(1)) {
                    let props = init.borrow().props.clone();
                    o.borrow_mut().props = props;
                }
                self.py_register_dict(&d, PyDict::Default(a0));
                d
            }
            ("_collections", "OrderedDict") => {
                let d = V::new_obj();
                if !matches!(a0, V::Null) {
                    let items = match &a0 {
                        V::Obj(o) => o.borrow().props.iter().map(|(k, v)| V::arr(vec![V::Str(k.clone()), v.clone()])).collect(),
                        other => self.iterate(other, Some(pos))?,
                    };
                    for p in items {
                        let kv = self.iterate(&p, Some(pos))?;
                        if let (Some(k), Some(v)) = (kv.first(), kv.get(1)) {
                            self.set_index(&d, k, v.clone(), pos)?;
                        }
                    }
                }
                self.py_register_dict(&d, PyDict::Ordered);
                d
            }
            ("_collections", "namedtuple_type") => self.namedtuple_type(&a0, &a1, args.get(2).cloned().unwrap_or(V::Null), pos)?,
            // ---------------- dataclasses
            ("dataclasses", "field") => {
                // only reached for class attributes; __init__ handles the options
                let kwa = match args.last() {
                    Some(V::Obj(o)) if o.borrow().kind == ObjKind::Kwargs => Some(o.clone()),
                    _ => None,
                };
                kwa.and_then(|o| o.borrow().props.get("default").cloned()).unwrap_or(V::Null)
            }
            ("dataclasses", "dataclass") => {
                // decorator with options is resolved at parse time: identity at runtime
                if a0.is_callable() {
                    a0
                } else {
                    V::Method(Rc::new((V::Null, "%identity".into())))
                }
            }
            ("dataclasses", "is_dataclass") => V::Bool(self.dc_fields(&a0).is_some()),
            ("dataclasses", "fields") => {
                let names = self.dc_fields(&a0).unwrap_or_default();
                V::arr(
                    names
                        .into_iter()
                        .map(|n| {
                            let mut p = PropMap::default();
                            p.set("name", V::string(n));
                            V::obj(p)
                        })
                        .collect(),
                )
            }
            ("dataclasses", "asdict") => self.dc_asdict(&a0, false, pos)?,
            ("dataclasses", "astuple") => self.dc_asdict(&a0, true, pos)?,
            ("dataclasses", "replace") => {
                let Some(names) = self.dc_fields(&a0) else {
                    return self.throw("TypeError", "replace() should be called on dataclass instances".into(), Some(pos));
                };
                let mut kw = PropMap::default();
                for n in &names {
                    kw.set(n, self.get_member(&a0, n, pos)?);
                }
                if let Some(V::Obj(o)) = args.last() {
                    if o.borrow().kind == ObjKind::Kwargs {
                        for (k, v) in o.borrow().props.iter() {
                            kw.set(k, v.clone());
                        }
                    }
                }
                let cls = crate::classes::class_of(&a0).map(V::Func).unwrap_or(V::Null);
                let kwv = V::obj_kind(kw, ObjKind::Kwargs);
                self.call(&cls, V::Undef, vec![kwv], pos)?
            }
            // ---------------- asyncio
            ("asyncio", "run") => {
                let p = self.promise_resolve(a0)?;
                self.drain_jobs()?;
                match crate::promise::settled(&p) {
                    Some(crate::promise::PState::Fulfilled(v)) => v,
                    Some(crate::promise::PState::Rejected(e)) => return Err(Ctrl::Throw(e)),
                    _ => return self.throw("RuntimeError", "asyncio.run(): the coroutine never completed".into(), Some(pos)),
                }
            }
            ("asyncio", "sleep") => {
                let p = self.new_promise();
                let secs = a0.as_f64().unwrap_or(0.0);
                let res = V::Method(Rc::new((p.clone(), "%adopt_ok".into())));
                self.add_timer(res, secs * 1000.0, vec![a1], false);
                p
            }
            ("asyncio", "gather") => {
                let mut ps = Vec::new();
                for a in args.iter().filter(|a| !matches!(a, V::Obj(o) if o.borrow().kind == ObjKind::Kwargs)) {
                    ps.push(self.promise_resolve(a.clone())?);
                }
                self.promise_static("all", vec![V::arr(ps)], pos)?
            }
            ("asyncio", "create_task" | "ensure_future") => self.promise_resolve(a0)?,
            ("asyncio", "wait_for") => self.promise_resolve(a0)?,
            ("asyncio", "iscoroutine") => V::Bool(coroutine_of(&a0).is_some()),
            ("asyncio", "iscoroutinefunction") => V::Bool(matches!(&a0, V::Func(c) if c.def.is_async)),
            // ---------------- operator
            ("operator", "itemgetter") => {
                let mut p = PropMap::default();
                p.set("keys", V::arr(args.clone()));
                V::Method(Rc::new((V::obj(p), "%itemgetter".into())))
            }
            ("operator", "attrgetter") => {
                let mut p = PropMap::default();
                p.set("keys", V::arr(args.clone()));
                V::Method(Rc::new((V::obj(p), "%attrgetter".into())))
            }
            ("operator", "add") => self.binary(crate::ast::BinOp::Add, a0, a1, pos)?,
            ("operator", "sub") => self.binary(crate::ast::BinOp::Sub, a0, a1, pos)?,
            ("operator", "mul") => self.binary(crate::ast::BinOp::Mul, a0, a1, pos)?,
            ("operator", "truediv") => self.binary(crate::ast::BinOp::Div, a0, a1, pos)?,
            ("operator", "floordiv") => self.binary(crate::ast::BinOp::FloorDiv, a0, a1, pos)?,
            ("operator", "mod") => self.binary(crate::ast::BinOp::Mod, a0, a1, pos)?,
            ("operator", "eq") => V::Bool(self.equals(&a0, &a1)),
            ("operator", "lt") => self.binary(crate::ast::BinOp::Lt, a0, a1, pos)?,
            ("operator", "neg") => self.binary(crate::ast::BinOp::Sub, V::Int(0), a0, pos)?,
            ("operator", "not_") => V::Bool(!self.truthy(&a0)),
            ("operator", "getitem") => self.get_index(&a0, &a1, pos)?,
            // ---------------- copy
            ("copy", "copy") => match &a0 {
                V::Arr(a) => V::arr(a.borrow().clone()),
                V::Obj(o) => {
                    let b = o.borrow();
                    V::Obj(new_obj_ref(Obj { props: b.props.clone(), proto: b.proto.clone(), kind: b.kind.clone() }))
                }
                other => other.clone(),
            },
            ("copy", "deepcopy") => deep_copy(&a0, 0),
            // ---------------- typing / abc
            ("typing", "cast") => a1,
            ("typing", "TypeVar" | "NewType") => V::host(Host::Type("object".into())),
            ("abc", "abstractmethod") | ("typing", "overload" | "final" | "runtime_checkable") => a0,
            // ---------------- time / random (deterministic enough for scripts)
            ("time", "time") => crate::interp::norm_num(camel_core::util_time::now_millis() as f64 / 1000.0),
            ("time", "monotonic" | "perf_counter") => crate::interp::norm_num(self.queues.now / 1000.0),
            ("time", "sleep") => V::Null,
            ("random", "random") => crate::interp::norm_num((camel_core::language::support::random_u64() >> 11) as f64 / (1u64 << 53) as f64),
            ("random", "randint") => {
                let lo = a0.as_f64().unwrap_or(0.0) as i64;
                let hi = a1.as_f64().unwrap_or(0.0) as i64;
                V::Int(lo + (camel_core::language::support::random_u64() % ((hi - lo + 1).max(1) as u64)) as i64)
            }
            ("random", "choice") => {
                let items = self.iterate(&a0, Some(pos))?;
                if items.is_empty() {
                    return self.throw("IndexError", "Cannot choose from an empty sequence".into(), Some(pos));
                }
                items[(camel_core::language::support::random_u64() % items.len() as u64) as usize].clone()
            }
            _ => return Ok(None),
        }))
    }

    /// Internal (`%`-named) methods used by the Python runtime.
    pub fn py_internal(&mut self, recv: &V, name: &str, args: &[V], env_pos: u32) -> R<Option<V>> {
        let pos = env_pos;
        let a0 = args.first().cloned().unwrap_or(V::Null);
        let a1 = args.get(1).cloned().unwrap_or(V::Null);
        Ok(Some(match name {
            "%pyimport" => {
                let n = self.display(&a0);
                let top = self.truthy(&a1);
                self.py_import(&n, top, pos)?
            }
            "%pyfrom" => {
                let m = self.display(&a0);
                let n = self.display(&a1);
                self.py_from(&m, &n, pos)?
            }
            "%metaclass" => self.py_apply_metaclass(&a0, &a1, pos)?,
            "%match_seq" => V::Bool(matches!(a0, V::Arr(_)) || matches!(&a0, V::Obj(o) if o.borrow().proto.is_some() && self.user_method(&a0, "__getitem__").is_some() && self.user_method(&a0, "__len__").is_some())),
            "%match_map" => V::Bool(matches!(&a0, V::Obj(o) if matches!(o.borrow().kind, ObjKind::Plain | ObjKind::Kwargs) && o.borrow().proto.is_none())),
            "%match_rest" => {
                let keys: Vec<String> = self.iterate(&a1, Some(pos))?.iter().map(|k| self.key_string(k)).collect();
                let mut p = PropMap::default();
                if let V::Obj(o) = &a0 {
                    for (k, v) in o.borrow().props.iter() {
                        if !keys.iter().any(|x| **x == **k) {
                            p.set(k, v.clone());
                        }
                    }
                }
                V::obj(p)
            }
            "%match_pos" => {
                let i = a1.as_f64().unwrap_or(0.0) as usize;
                let cls = args.get(1).cloned().unwrap_or(V::Null);
                let idx = args.get(2).and_then(|v| v.as_f64()).unwrap_or(i as f64) as usize;
                match &cls {
                    V::Host(_) => {
                        // int(x), str(x)...: the subject itself
                        if idx == 0 {
                            a0
                        } else {
                            return self.throw("TypeError", "too many positional sub-patterns".into(), Some(pos));
                        }
                    }
                    _ => {
                        let ma = self.get_member(&cls, "__match_args__", pos).unwrap_or(V::Null);
                        let names = if ma.is_nullish() { vec![] } else { self.iterate(&ma, Some(pos))? };
                        match names.get(idx) {
                            Some(n) => {
                                let n = self.display(n);
                                self.get_member(&a0, &n, pos)?
                            }
                            None => return self.throw("TypeError", format!("{}() accepts {} positional sub-patterns", self.display(&cls), names.len()), Some(pos)),
                        }
                    }
                }
            }
            "%identity" => a0,
            "%itemgetter" | "%attrgetter" => {
                let keys = match recv {
                    V::Obj(o) => o.borrow().props.get("keys").cloned().unwrap_or(V::Null),
                    _ => V::Null,
                };
                let keys = self.iterate(&keys, Some(pos))?;
                let mut out = Vec::new();
                for k in &keys {
                    out.push(if name == "%itemgetter" {
                        self.get_index(&a0, k, pos)?
                    } else {
                        let mut cur = a0.clone();
                        for part in self.display(k).split('.') {
                            cur = self.get_member(&cur, part, pos)?;
                        }
                        cur
                    });
                }
                if out.len() == 1 {
                    out.pop().unwrap()
                } else {
                    V::arr(out)
                }
            }
            "%nt_new" => self.namedtuple_new(recv, args, pos)?,
            "%type_call" => {
                // type.__call__ / super().__call__ inside a metaclass
                let cls = a0.clone();
                match &cls {
                    V::Func(c) => self.construct_class(c, args[1..].to_vec(), pos)?,
                    _ => V::Null,
                }
            }
            _ => return Ok(None),
        }))
    }
}

fn deep_copy(v: &V, depth: usize) -> V {
    if depth > 100 {
        return v.clone();
    }
    match v {
        V::Arr(a) => V::arr(a.borrow().iter().map(|x| deep_copy(x, depth + 1)).collect()),
        V::Obj(o) => {
            let b = o.borrow();
            if matches!(b.kind, ObjKind::Plain | ObjKind::Set | ObjKind::Kwargs) {
                let mut p = PropMap::default();
                for (k, x) in b.props.iter() {
                    p.set(k, deep_copy(x, depth + 1));
                }
                V::Obj(new_obj_ref(Obj { props: p, proto: b.proto.clone(), kind: b.kind.clone() }))
            } else {
                v.clone()
            }
        }
        other => other.clone(),
    }
}

// ====================================================================== collections

impl Interp {
    fn py_register_dict(&mut self, d: &V, kind: PyDict) {
        if let V::Obj(o) = d {
            self.py_dicts.insert(Rc::as_ptr(o) as *const u8 as usize, (Rc::downgrade(o), kind));
        }
    }

    /// The `collections` kind of a dict.
    pub fn py_dict_kind(&self, o: &ObjRef) -> Option<PyDict> {
        if self.py_dicts.is_empty() {
            return None;
        }
        let key = Rc::as_ptr(o) as *const u8 as usize;
        self.py_dicts.get(&key).filter(|(w, _)| w.upgrade().is_some_and(|x| Rc::ptr_eq(&x, o))).map(|(_, k)| k.clone())
    }

    /// `d[missing]` of a `defaultdict` / `Counter`.
    pub fn py_dict_missing(&mut self, o: &ObjRef, key: &V, pos: u32) -> R<Option<V>> {
        match self.py_dict_kind(o) {
            Some(PyDict::Default(f)) if !f.is_nullish() => {
                let v = self.call(&f, V::Undef, vec![], pos)?;
                let k = self.key_string(key);
                o.borrow_mut().props.set(&k, v.clone());
                Ok(Some(v))
            }
            Some(PyDict::Counter) => Ok(Some(V::Int(0))),
            _ => Ok(None),
        }
    }

    fn counter_update(&mut self, d: &V, args: &[V], sign: i64, pos: u32) -> R<()> {
        let mut src: Vec<(V, i64)> = Vec::new();
        for a in args {
            match a {
                V::Obj(o) if matches!(o.borrow().kind, ObjKind::Plain | ObjKind::Kwargs) && o.borrow().proto.is_none() => {
                    for (k, v) in o.borrow().props.iter() {
                        src.push((V::Str(k.clone()), v.as_f64().unwrap_or(0.0) as i64));
                    }
                }
                V::Null => {}
                other => {
                    for x in self.iterate(other, Some(pos))? {
                        src.push((x, 1));
                    }
                }
            }
        }
        if let V::Obj(o) = d {
            for (k, n) in src {
                let key = self.key_string(&k);
                let cur = o.borrow().props.get(&key).and_then(|v| v.as_f64()).unwrap_or(0.0) as i64;
                o.borrow_mut().props.set(&key, V::Int(cur + sign * n));
            }
        }
        Ok(())
    }

    fn counter_pairs(&self, o: &ObjRef) -> Vec<(Rc<str>, i64)> {
        let mut pairs: Vec<(Rc<str>, i64)> = o.borrow().props.iter().map(|(k, v)| (k.clone(), v.as_f64().unwrap_or(0.0) as i64)).collect();
        // stable: equal counts keep insertion order
        pairs.sort_by(|a, b| b.1.cmp(&a.1));
        pairs
    }

    /// Methods of `Counter` / `defaultdict` / `OrderedDict` dicts.
    pub fn py_dict_method(&mut self, o: &ObjRef, recv: &V, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
        let Some(kind) = self.py_dict_kind(o) else { return Ok(None) };
        let a0 = args.first().cloned().unwrap_or(V::Null);
        match (&kind, name) {
            (PyDict::Counter, "most_common") => {
                let pairs = self.counter_pairs(o);
                let n = if a0.is_nullish() { pairs.len() } else { a0.as_f64().unwrap_or(0.0).max(0.0) as usize };
                Ok(Some(V::arr(pairs.into_iter().take(n).map(|(k, c)| V::arr(vec![V::Str(k), V::Int(c)])).collect())))
            }
            (PyDict::Counter, "elements") => {
                let mut out = vec![];
                for (k, v) in o.borrow().props.iter() {
                    for _ in 0..v.as_f64().unwrap_or(0.0).max(0.0) as usize {
                        out.push(V::Str(k.clone()));
                    }
                }
                Ok(Some(V::arr(out)))
            }
            (PyDict::Counter, "update") => {
                self.counter_update(recv, args, 1, pos)?;
                Ok(Some(V::Null))
            }
            (PyDict::Counter, "subtract") => {
                self.counter_update(recv, args, -1, pos)?;
                Ok(Some(V::Null))
            }
            (PyDict::Counter, "total") => Ok(Some(V::Int(o.borrow().props.values().map(|v| v.as_f64().unwrap_or(0.0) as i64).sum()))),
            (_, "copy") => {
                let d = V::obj(o.borrow().props.clone());
                self.py_register_dict(&d, kind.clone());
                Ok(Some(d))
            }
            (PyDict::Ordered, "move_to_end") => {
                let k = self.key_string(&a0);
                let last = args.get(1).map(|v| self.truthy(v)).unwrap_or(true);
                let v = o.borrow_mut().props.remove(&k);
                if let Some(v) = v {
                    if last {
                        o.borrow_mut().props.set(&k, v);
                    } else {
                        let mut p = PropMap::default();
                        p.set(&k, v);
                        for (kk, vv) in o.borrow().props.iter() {
                            p.set(kk, vv.clone());
                        }
                        o.borrow_mut().props = p;
                    }
                } else {
                    return self.throw("KeyError", crate::builtins::py_repr(self, &a0), Some(pos)).map(Some);
                }
                Ok(Some(V::Null))
            }
            (PyDict::Ordered, "popitem") => {
                let last = args.first().map(|v| self.truthy(v)).unwrap_or(true);
                let key = {
                    let b = o.borrow();
                    let keys: Vec<Rc<str>> = b.props.keys().cloned().collect();
                    if last { keys.last().cloned() } else { keys.first().cloned() }
                };
                match key {
                    Some(k) => {
                        let v = o.borrow_mut().props.remove(&k).unwrap_or(V::Null);
                        Ok(Some(V::arr(vec![V::Str(k), v])))
                    }
                    None => self.throw("KeyError", "'dictionary is empty'".into(), Some(pos)).map(Some),
                }
            }
            _ => Ok(None),
        }
    }

    /// `repr()` of the `collections` dicts.
    pub fn py_dict_display(&self, o: &ObjRef, inner: String) -> Option<String> {
        Some(match self.py_dict_kind(o)? {
            PyDict::Counter => {
                let pairs = self.counter_pairs(o);
                let body = pairs.iter().map(|(k, c)| format!("{}: {}", crate::builtins::py_repr(self, &V::Str(k.clone())), c)).collect::<Vec<_>>().join(", ");
                format!("Counter({{{}}})", body)
            }
            PyDict::Default(f) => {
                let fname = match &f {
                    V::Host(h) => match &**h {
                        Host::Ns(n) => format!("<class '{}'>", n),
                        _ => "None".into(),
                    },
                    V::Func(c) => format!("<function {}>", c.def.name.clone().unwrap_or_else(|| "<lambda>".into())),
                    _ => "None".into(),
                };
                format!("defaultdict({}, {})", fname, inner)
            }
            PyDict::Ordered => format!("OrderedDict({})", inner),
        })
    }

    /// `Counter` arithmetic (`+`, `-`, `&`, `|`).
    pub fn py_counter_binary(&mut self, op: crate::ast::BinOp, a: &V, b: &V) -> Option<V> {
        let (V::Obj(x), V::Obj(y)) = (a, b) else { return None };
        if !matches!(self.py_dict_kind(x), Some(PyDict::Counter)) || !matches!(self.py_dict_kind(y), Some(PyDict::Counter)) {
            return None;
        }
        use crate::ast::BinOp::*;
        let get = |o: &ObjRef, k: &str| o.borrow().props.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0) as i64;
        let mut keys: Vec<Rc<str>> = x.borrow().props.keys().cloned().collect();
        for k in y.borrow().props.keys() {
            if !keys.contains(k) {
                keys.push(k.clone());
            }
        }
        let mut p = PropMap::default();
        for k in keys {
            let (m, n) = (get(x, &k), get(y, &k));
            let r = match op {
                Add => m + n,
                Sub => m - n,
                BitAnd => m.min(n),
                BitOr => m.max(n),
                _ => return None,
            };
            if r > 0 {
                p.set(&k, V::Int(r));
            }
        }
        let d = V::obj(p);
        self.py_register_dict(&d, PyDict::Counter);
        Some(d)
    }

    // ---------------------------------------------------------------- namedtuple

    fn namedtuple_type(&mut self, name: &V, fields: &V, defaults: V, pos: u32) -> R<V> {
        let mut p = PropMap::default();
        p.set("__name__", name.clone());
        let fs = self.iterate(fields, Some(pos))?;
        p.set("_fields", V::arr(fs));
        let defs = if defaults.is_nullish() { vec![] } else { self.iterate(&defaults, Some(pos))? };
        p.set("_field_defaults", V::arr(defs));
        let spec = V::obj(p);
        Ok(V::Method(Rc::new((spec, "%nt_new".into()))))
    }

    fn namedtuple_new(&mut self, spec: &V, args: &[V], pos: u32) -> R<V> {
        let (name, fields, defaults) = match spec {
            V::Obj(o) => {
                let b = o.borrow();
                (b.props.get("__name__").cloned().unwrap_or(V::Null), b.props.get("_fields").cloned().unwrap_or(V::Null), b.props.get("_field_defaults").cloned().unwrap_or(V::Null))
            }
            _ => return Ok(V::Null),
        };
        let fields = self.iterate(&fields, Some(pos))?;
        let defaults = self.iterate(&defaults, Some(pos))?;
        let mut args = args.to_vec();
        let kw = match args.last() {
            Some(V::Obj(o)) if o.borrow().kind == ObjKind::Kwargs => {
                let o = o.clone();
                args.pop();
                Some(o)
            }
            _ => None,
        };
        let n = fields.len();
        let mut vals = Vec::with_capacity(n);
        for (i, f) in fields.iter().enumerate() {
            let fname = self.display(f);
            let v = match args.get(i) {
                Some(v) => v.clone(),
                None => match kw.as_ref().and_then(|o| o.borrow().props.get(&fname).cloned()) {
                    Some(v) => v,
                    None => {
                        let di = i as i64 - (n as i64 - defaults.len() as i64);
                        if di >= 0 {
                            defaults[di as usize].clone()
                        } else {
                            return self.throw("TypeError", format!("{}() missing required argument: '{}'", self.display(&name), fname), Some(pos));
                        }
                    }
                },
            };
            vals.push(v);
        }
        let t = V::arr(vals);
        crate::jsext::set_arr_prop(self, &t, "%nt", spec.clone());
        Ok(t)
    }

    /// Attributes / methods of a namedtuple instance (a tuple with field names).
    pub fn namedtuple_member(&mut self, a: &ArrRef, name: &str, pos: u32) -> R<Option<V>> {
        let Some(spec) = crate::jsext::arr_prop(self, a, "%nt") else { return Ok(None) };
        let fields = match &spec {
            V::Obj(o) => o.borrow().props.get("_fields").cloned().unwrap_or(V::Null),
            _ => V::Null,
        };
        let fields: Vec<String> = self.iterate(&fields, Some(pos))?.iter().map(|f| self.display(f)).collect();
        if let Some(i) = fields.iter().position(|f| f == name) {
            return Ok(a.borrow().get(i).cloned());
        }
        match name {
            "_fields" => Ok(Some(V::arr(fields.iter().map(|f| V::str(f)).collect()))),
            "_asdict" | "_replace" => Ok(Some(V::Method(Rc::new((V::Arr(a.clone()), format!("%nt{}", name)))))),
            _ => Ok(None),
        }
    }

    /// `nt._asdict()` / `nt._replace(**kw)`.
    pub fn namedtuple_method(&mut self, a: &ArrRef, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
        let Some(spec) = crate::jsext::arr_prop(self, a, "%nt") else { return Ok(None) };
        let fields = match &spec {
            V::Obj(o) => o.borrow().props.get("_fields").cloned().unwrap_or(V::Null),
            _ => V::Null,
        };
        let fields: Vec<String> = self.iterate(&fields, Some(pos))?.iter().map(|f| self.display(f)).collect();
        let vals = a.borrow().clone();
        match name {
            "_asdict" | "%nt_asdict" => {
                let mut p = PropMap::default();
                for (f, v) in fields.iter().zip(vals) {
                    p.set(f, v);
                }
                Ok(Some(V::obj(p)))
            }
            "_replace" | "%nt_replace" => {
                let mut vals = vals;
                if let Some(V::Obj(o)) = args.last() {
                    for (k, v) in o.borrow().props.iter() {
                        if let Some(i) = fields.iter().position(|f| **f == **k) {
                            vals[i] = v.clone();
                        }
                    }
                }
                let t = V::arr(vals);
                crate::jsext::set_arr_prop(self, &t, "%nt", spec);
                Ok(Some(t))
            }
            _ => Ok(None),
        }
    }

    /// `repr()` of a namedtuple instance.
    pub fn namedtuple_display(&self, a: &ArrRef) -> Option<String> {
        let spec = crate::jsext::arr_prop(self, a, "%nt")?;
        let V::Obj(o) = &spec else { return None };
        let b = o.borrow();
        let name = b.props.get("__name__").map(|n| self.display(n)).unwrap_or_default();
        let fields: Vec<String> = match b.props.get("_fields") {
            Some(V::Arr(f)) => f.borrow().iter().map(|x| self.display(x)).collect(),
            _ => vec![],
        };
        let vals = a.borrow();
        let body = fields.iter().zip(vals.iter()).map(|(f, v)| format!("{}={}", f, crate::builtins::py_repr(self, v))).collect::<Vec<_>>().join(", ");
        Some(format!("{}({})", name, body))
    }

    // ---------------------------------------------------------------- dataclasses

    fn dc_fields(&mut self, v: &V) -> Option<Vec<String>> {
        let cls = match v {
            V::Func(c) if c.flavor == Flavor::Class => Some(c.clone()),
            other => crate::classes::class_of(other),
        }?;
        let proto = self.func_prototype(&cls)?;
        let f = match &proto {
            V::Obj(p) => p.borrow().lookup("__dataclass_fields__"),
            _ => None,
        }?;
        let items = self.iterate(&f, None).ok()?;
        Some(items.iter().map(|x| self.display(x)).collect())
    }

    fn dc_asdict(&mut self, v: &V, tuple: bool, pos: u32) -> R<V> {
        if let Some(names) = self.dc_fields(v) {
            if !matches!(v, V::Func(_)) {
                let mut p = PropMap::default();
                let mut t = Vec::new();
                for n in names {
                    let x = self.get_member(v, &n, pos)?;
                    let x = self.dc_asdict(&x, tuple, pos)?;
                    t.push(x.clone());
                    p.set(&n, x);
                }
                return Ok(if tuple { V::arr(t) } else { V::obj(p) });
            }
        }
        Ok(match v {
            V::Arr(a) => {
                let items = a.borrow().clone();
                let mut out = Vec::new();
                for x in items {
                    out.push(self.dc_asdict(&x, tuple, pos)?);
                }
                V::arr(out)
            }
            V::Obj(o) if o.borrow().proto.is_none() && o.borrow().kind == ObjKind::Plain => {
                let items: Vec<(Rc<str>, V)> = o.borrow().props.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                let mut p = PropMap::default();
                for (k, x) in items {
                    p.set(&k, self.dc_asdict(&x, tuple, pos)?);
                }
                V::obj(p)
            }
            other => other.clone(),
        })
    }

    /// After a Python instance was constructed: freeze `@dataclass(frozen=True)` instances.
    pub fn py_after_construct(&mut self, def: &ClassDef, this: &V) {
        if def.annotations.iter().any(|a| a == "frozen") {
            if let V::Obj(o) = this {
                self.frozen.insert(Rc::as_ptr(o) as *const u8 as usize, Rc::downgrade(o));
            }
        }
    }

    // ---------------------------------------------------------------- metaclasses

    /// Class creation hook: run `metaclass` (or the parent's metaclass) for a new class.
    fn py_apply_metaclass(&mut self, meta: &V, cls: &V, pos: u32) -> R<V> {
        let V::Func(c) = cls else { return Ok(cls.clone()) };
        let meta = if meta.is_nullish() {
            match &c.parent {
                Some(V::Func(p)) => p.props.borrow().get("%meta").cloned().unwrap_or(V::Null),
                _ => V::Null,
            }
        } else {
            meta.clone()
        };
        let V::Func(m) = &meta else { return Ok(cls.clone()) };
        if m.flavor != Flavor::Class {
            // a plain function used as metaclass: called with (name, bases, ns)
            let (name, bases, ns) = self.class_parts(c);
            return self.call(&meta, V::Undef, vec![name, bases, ns], pos);
        }
        c.props.borrow_mut().set("%meta", meta.clone());
        let (name, bases, ns) = self.class_parts(c);
        let mut result = cls.clone();
        if let Some(f) = self.meta_own(m, "__new__") {
            self.pending_class.push(cls.clone());
            let r = self.call(&f, V::Undef, vec![meta.clone(), name.clone(), bases.clone(), ns.clone()], pos);
            self.pending_class.pop();
            result = r?;
            if let V::Func(rc) = &result {
                rc.props.borrow_mut().set("%meta", meta.clone());
            }
        }
        if let Some(f) = self.meta_own(m, "__init__") {
            self.call(&f, V::Undef, vec![result.clone(), name, bases, ns], pos)?;
        }
        Ok(result)
    }

    /// A method defined by a metaclass (or its parents, excluding `type`).
    fn meta_own(&mut self, m: &Rc<Closure>, name: &str) -> Option<V> {
        match self.func_prototype(m) {
            Some(V::Obj(p)) => p.borrow().lookup(name).filter(|f| matches!(f, V::Func(_))),
            _ => None,
        }
    }

    /// (name, bases tuple, namespace dict) of a class.
    fn class_parts(&mut self, c: &Rc<Closure>) -> (V, V, V) {
        let name = V::str(c.def.name.as_deref().unwrap_or(""));
        let bases = match &c.parent {
            Some(p @ V::Func(_)) => V::arr(vec![p.clone()]),
            _ => V::arr(vec![]),
        };
        let mut ns = PropMap::default();
        if let Some(V::Obj(p)) = self.func_prototype(c) {
            for (k, v) in p.borrow().props.iter() {
                if &**k != "constructor" {
                    ns.set(k, v.clone());
                }
            }
        }
        (name, bases, V::obj(ns))
    }

    /// `type.__new__(mcs, name, bases, ns)`: the class being created with the namespace
    /// changes applied (or a new class for `type(name, bases, ns)`).
    pub fn py_type_new(&mut self, args: &[V], pos: u32) -> R<V> {
        let (name, bases, ns) = (args.get(1).cloned().unwrap_or(V::Null), args.get(2).cloned().unwrap_or(V::Null), args.get(3).cloned().unwrap_or(V::Null));
        if let Some(V::Func(c)) = self.pending_class.last().cloned() {
            // apply namespace changes to the class
            if let (Some(V::Obj(p)), V::Obj(nso)) = (self.func_prototype(&c), &ns) {
                let items: Vec<(Rc<str>, V)> = nso.borrow().props.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                for (k, v) in items {
                    let v = method_flavor(v);
                    p.borrow_mut().props.set(&k, v);
                }
            }
            return Ok(V::Func(c));
        }
        self.py_type3(&name, &bases, &ns, pos)
    }

    /// `type(name, bases, dict)`: create a class dynamically.
    pub fn py_type3(&mut self, name: &V, bases: &V, ns: &V, pos: u32) -> R<V> {
        let name = self.display(name);
        let bases = if bases.is_nullish() { vec![] } else { self.iterate(bases, Some(pos))? };
        let parent = bases.into_iter().find(|b| matches!(b, V::Func(c) if c.flavor == Flavor::Class));
        let proto = new_obj_ref(Obj::new());
        if let Some(V::Func(pc)) = &parent {
            if let Some(V::Obj(pp)) = self.func_prototype(pc) {
                proto.borrow_mut().proto = Some(pp);
            }
        }
        if let V::Obj(nso) = ns {
            for (k, v) in nso.borrow().props.iter() {
                proto.borrow_mut().props.set(k, method_flavor(v.clone()));
            }
        }
        let def = Arc::new(FuncDef::new(Some(name.clone()), vec![], None, FuncBody::Block(vec![])));
        let closure = new_closure(Closure { def, env: self.globals.clone(), props: RefCell::new(PropMap::default()), parent: parent.clone(), home: RefCell::new(Some(proto.clone())), flavor: Flavor::Class });
        let class_v = V::Func(closure.clone());
        proto.borrow_mut().props.set("constructor", class_v.clone());
        closure.props.borrow_mut().set("prototype", V::Obj(proto));
        closure.props.borrow_mut().set("name", V::str(&name));
        let cdef = Arc::new(ClassDef { name: Some(name), extends: None, ctors: vec![], methods: vec![], fields: vec![], mixins: vec![], style: ClassStyle::Python, annotations: vec![] });
        let error_base = match &parent {
            Some(V::Func(pc)) => self.classes.get(&(Rc::as_ptr(pc) as usize)).and_then(|i| i.error_base.clone()),
            _ => None,
        };
        self.classes.insert(Rc::as_ptr(&closure) as usize, crate::classes::ClassInfo { def: cdef, env: self.globals.clone(), error_base, mixin_fields: vec![] });
        Ok(class_v)
    }

    /// Calling a class whose metaclass defines `__call__`.
    pub fn py_meta_call(&mut self, c: &Rc<Closure>, args: &[V], pos: u32) -> R<Option<V>> {
        let meta = c.props.borrow().get("%meta").cloned();
        let Some(V::Func(m)) = meta else { return Ok(None) };
        let Some(f) = self.meta_own(&m, "__call__") else { return Ok(None) };
        let mut a = vec![V::Func(c.clone())];
        a.extend(args.iter().cloned());
        self.call(&f, V::Undef, a, pos).map(Some)
    }

    /// Class attribute lookup falling back to the metaclass (methods bound to the class).
    pub fn py_meta_attr(&mut self, c: &Rc<Closure>, name: &str) -> Option<V> {
        let meta = c.props.borrow().get("%meta").cloned();
        let Some(V::Func(m)) = meta else { return None };
        let f = match self.func_prototype(&m) {
            Some(V::Obj(p)) => p.borrow().lookup(name),
            _ => None,
        }?;
        match &f {
            V::Func(k) if k.flavor == Flavor::PyMethod => {
                let mut p = PropMap::default();
                p.set("fn", f.clone());
                p.set("this", V::Undef);
                p.set("args", V::arr(vec![V::Func(c.clone())]));
                Some(V::Method(Rc::new((V::obj(p), "%bound".to_string()))))
            }
            _ => Some(f),
        }
    }
}

/// Functions put into a dynamically created class become methods (bound to `self`).
fn method_flavor(v: V) -> V {
    match &v {
        V::Func(c) if c.flavor == Flavor::Plain => V::Func(new_closure(Closure {
            def: c.def.clone(),
            env: c.env.clone(),
            props: RefCell::new(c.props.borrow().clone()),
            parent: c.parent.clone(),
            home: RefCell::new(c.home.borrow().clone()),
            flavor: Flavor::PyMethod,
        })),
        _ => v,
    }
}

/// The generator state of a Python coroutine object (an un-started `async def` call).
pub fn coroutine_of(v: &V) -> Option<Internal<crate::gen::GenState>> {
    match v {
        V::Obj(o) => match &o.borrow().kind {
            ObjKind::Generator(g) if g.0.borrow().coroutine => Some(g.clone()),
            _ => None,
        },
        _ => None,
    }
}

impl Interp {
    /// Python ordering for sorting: `__lt__` of class instances, tuples element-wise.
    pub fn py_order(&mut self, a: &V, b: &V, pos: u32) -> R<std::cmp::Ordering> {
        use std::cmp::Ordering;
        if self.d != Dialect::Python {
            return Ok(crate::builtins::natural(self, a, b));
        }
        let is_inst = |v: &V| matches!(v, V::Obj(o) if o.borrow().proto.is_some());
        if is_inst(a) || is_inst(b) {
            if let Some(f) = self.user_method(a, "__lt__") {
                let r = self.call_user_method(a, &f, vec![b.clone()], pos)?;
                if self.truthy(&r) {
                    return Ok(Ordering::Less);
                }
                let r2 = self.call_user_method(b, &self.user_method(b, "__lt__").unwrap_or(f), vec![a.clone()], pos)?;
                return Ok(if self.truthy(&r2) { Ordering::Greater } else { Ordering::Equal });
            }
        }
        if let (V::Arr(x), V::Arr(y)) = (a, b) {
            let (x, y) = (x.borrow().clone(), y.borrow().clone());
            for (p, q) in x.iter().zip(y.iter()) {
                match self.py_order(p, q, pos)? {
                    Ordering::Equal => continue,
                    o => return Ok(o),
                }
            }
            return Ok(x.len().cmp(&y.len()));
        }
        Ok(crate::builtins::natural(self, a, b))
    }
}

