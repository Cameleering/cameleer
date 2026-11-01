//! DataSonnet / Jsonnet objects with late binding.
//!
//! An object is a stack of layers (one per object literal combined with `+`); every field is
//! evaluated with `self` bound to the whole (combined) object, `super` to the layers below the
//! field's own layer and `$` to the outermost object. Visible fields are materialized into the
//! object's properties (so the generic object machinery sees them); hidden fields (`::`) are
//! kept apart and evaluated on access. `+:` merges a field with its inherited value.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use crate::ast::*;
use crate::interp::{new_scope, Env, Interp, R};
use crate::value::*;

/// A field definition of a layer.
pub struct JField {
    pub key: String,
    pub vis: Vis,
    pub plus: bool,
    pub val: FieldVal,
}

/// Where a field's value comes from.
pub enum FieldVal {
    /// Field `i` of an object literal.
    Def(Arc<JObjDef>, usize),
    /// A plain (already evaluated) value, e.g. from a parsed payload.
    Const(V),
}

/// One object literal (or plain object) in an inheritance chain.
pub struct JLayer {
    pub fields: Vec<JField>,
    pub def: Option<Arc<JObjDef>>,
    /// Environment of the literal.
    pub env: Option<Env>,
    /// `$` of an enclosing object (None: this literal is outermost, `$` is `self`).
    pub dollar: Option<V>,
}

impl JLayer {
    fn field(&self, k: &str) -> Option<&JField> {
        self.fields.iter().find(|f| f.key == k)
    }
}

/// Object metadata.
pub struct JMeta {
    pub layers: Rc<Vec<Rc<JLayer>>>,
    /// Number of visible layers (a `super` view sees fewer).
    pub upto: usize,
    /// `super` views: the object `self` refers to.
    pub self_obj: Option<V>,
    cache: HashMap<String, V>,
    busy: HashSet<String>,
    scopes: HashMap<usize, Env>,
    /// Visible fields whose evaluation failed when the object was created (e.g. a `super`
    /// reference resolved only after a later `+`): evaluated again when the object is used.
    pending: Vec<String>,
}

fn meta_of(o: &ObjRef) -> Option<Internal<JMeta>> {
    match &o.borrow().kind {
        ObjKind::Jsonnet(m) => Some(m.clone()),
        _ => None,
    }
}

impl Interp {
    /// Evaluate a DataSonnet object literal.
    pub fn eval_jobject(&mut self, def: &Arc<JObjDef>, env: &Env) -> R<V> {
        let mut fields = Vec::new();
        for (i, f) in def.fields.iter().enumerate() {
            let key = match &f.key {
                PropKey::Name(n) => n.clone(),
                PropKey::Computed(e) => {
                    let k = self.eval(e, env)?;
                    match k {
                        V::Str(s) => s.to_string(),
                        V::Null | V::Undef => continue,
                        other => return self.throw("RUNTIME ERROR", format!("field name must be string, got {}", crate::builtins::jsonnet_type(&other)), None),
                    }
                }
            };
            if fields.iter().any(|x: &JField| x.key == key) {
                return self.throw("RUNTIME ERROR", format!("duplicate field name: \"{}\"", key), None);
            }
            fields.push(JField { key, vis: f.vis, plus: f.plus, val: FieldVal::Def(def.clone(), i) });
        }
        let layer = JLayer { fields, def: Some(def.clone()), env: Some(env.clone()), dollar: env.lookup("$") };
        self.make_jobject(vec![Rc::new(layer)])
    }

    fn const_layer(&self, o: &ObjRef) -> JLayer {
        let fields = o.borrow().props.iter().map(|(k, v)| JField { key: k.to_string(), vis: Vis::Default, plus: false, val: FieldVal::Const(v.clone()) }).collect();
        JLayer { fields, def: None, env: None, dollar: None }
    }

    fn layers_of(&self, o: &ObjRef) -> Vec<Rc<JLayer>> {
        match meta_of(o) {
            Some(m) => {
                let b = m.0.borrow();
                b.layers[..b.upto].to_vec()
            }
            None => vec![Rc::new(self.const_layer(o))],
        }
    }

    /// `a + b` for DataSonnet objects.
    pub fn jobject_merge(&mut self, a: &ObjRef, b: &ObjRef) -> R<V> {
        let mut layers = self.layers_of(a);
        layers.extend(self.layers_of(b));
        self.make_jobject(layers)
    }

    fn make_jobject(&mut self, layers: Vec<Rc<JLayer>>) -> R<V> {
        let n = layers.len();
        let meta = JMeta { layers: Rc::new(layers), upto: n, self_obj: None, cache: HashMap::new(), busy: HashSet::new(), scopes: HashMap::new(), pending: Vec::new() };
        let o = new_obj_ref(Obj { props: PropMap::default(), proto: None, kind: ObjKind::Jsonnet(Internal::new(meta)) });
        let v = V::Obj(o.clone());
        // final visibility of every field
        let mut keys: Vec<(String, bool)> = Vec::new();
        {
            let m = meta_of(&o).unwrap();
            let b = m.0.borrow();
            for l in b.layers.iter() {
                for f in &l.fields {
                    match keys.iter_mut().find(|(k, _)| *k == f.key) {
                        Some((_, vis)) => match f.vis {
                            Vis::Hidden => *vis = false,
                            Vis::Visible => *vis = true,
                            Vis::Default => {}
                        },
                        None => keys.push((f.key.clone(), f.vis != Vis::Hidden)),
                    }
                }
            }
        }
        keys.sort();
        // object assertions
        let layers = meta_of(&o).unwrap().0.borrow().layers.clone();
        for (i, l) in layers.iter().enumerate() {
            if let Some(def) = &l.def {
                if !def.asserts.is_empty() {
                    let scope = self.layer_scope(&v, &o, i)?;
                    for (c, m, pos) in &def.asserts {
                        let cv = self.eval(c, &scope)?;
                        if !self.truthy(&cv) {
                            let mv = self.eval(m, &scope)?;
                            let msg = self.display(&mv);
                            return self.throw("RUNTIME ERROR", msg, Some(*pos));
                        }
                    }
                }
            }
        }
        let mut pending = Vec::new();
        for (k, visible) in keys {
            if visible {
                match self.jfield(&o, &k) {
                    Ok(Some(x)) => o.borrow_mut().props.set(&k, x),
                    Ok(None) => {}
                    Err(crate::interp::Ctrl::Throw(_)) => pending.push(k),
                    Err(e) => return Err(e),
                }
            }
        }
        if !pending.is_empty() {
            meta_of(&o).unwrap().0.borrow_mut().pending = pending;
        }
        Ok(v)
    }

    /// Evaluate the pending visible fields of a DataSonnet object (errors surface here).
    pub fn jcomplete(&mut self, o: &ObjRef) -> R<()> {
        let Some(m) = meta_of(o) else { return Ok(()) };
        let pending = std::mem::take(&mut m.0.borrow_mut().pending);
        for k in pending {
            if let Some(x) = self.jfield(o, &k)? {
                o.borrow_mut().props.set(&k, x);
            }
        }
        Ok(())
    }

    /// Scope for evaluating fields of layer `i` with `self` = `selfv`.
    fn layer_scope(&mut self, selfv: &V, selfo: &ObjRef, i: usize) -> R<Env> {
        let m = meta_of(selfo).unwrap();
        if let Some(e) = m.0.borrow().scopes.get(&i) {
            return Ok(e.clone());
        }
        let layer = m.0.borrow().layers[i].clone();
        let parent = layer.env.clone().unwrap_or_else(|| self.globals.clone());
        let scope = new_scope(Some(parent), false);
        scope.define("self", selfv.clone());
        let sup = {
            let b = m.0.borrow();
            JMeta { layers: b.layers.clone(), upto: i, self_obj: Some(selfv.clone()), cache: HashMap::new(), busy: HashSet::new(), scopes: HashMap::new(), pending: Vec::new() }
        };
        scope.define("super", V::Obj(new_obj_ref(Obj { props: PropMap::default(), proto: None, kind: ObjKind::Jsonnet(Internal::new(sup)) })));
        scope.define("$", layer.dollar.clone().unwrap_or_else(|| selfv.clone()));
        m.0.borrow_mut().scopes.insert(i, scope.clone());
        if let Some(def) = &layer.def {
            for (n, e) in &def.locals {
                let v = self.eval(e, &scope)?;
                scope.define(n, v);
            }
        }
        Ok(scope)
    }

    /// The value of field `k` of a DataSonnet object (or `super` view), hidden fields included.
    pub fn jfield(&mut self, o: &ObjRef, k: &str) -> R<Option<V>> {
        let Some(m) = meta_of(o) else {
            return Ok(o.borrow().props.get(k).cloned());
        };
        let (layers, upto, self_obj) = {
            let b = m.0.borrow();
            if let Some(v) = b.cache.get(k) {
                return Ok(Some(v.clone()));
            }
            (b.layers.clone(), b.upto, b.self_obj.clone())
        };
        let Some(i) = (0..upto).rev().find(|i| layers[*i].field(k).is_some()) else { return Ok(None) };
        let (selfv, selfo) = match &self_obj {
            Some(s @ V::Obj(so)) => (s.clone(), so.clone()),
            _ => (V::Obj(o.clone()), o.clone()),
        };
        if self_obj.is_none() {
            if m.0.borrow().busy.contains(k) {
                return self.throw("RUNTIME ERROR", format!("infinite recursion evaluating field '{}'", k), None);
            }
            m.0.borrow_mut().busy.insert(k.to_string());
        }
        let r = self.eval_layer_field(&selfv, &selfo, &layers, i, k);
        if self_obj.is_none() {
            m.0.borrow_mut().busy.remove(k);
        }
        let v = r?;
        if self_obj.is_none() {
            m.0.borrow_mut().cache.insert(k.to_string(), v.clone());
        }
        Ok(Some(v))
    }

    fn eval_layer_field(&mut self, selfv: &V, selfo: &ObjRef, layers: &Rc<Vec<Rc<JLayer>>>, i: usize, k: &str) -> R<V> {
        let f = layers[i].field(k).unwrap();
        let mut v = match &f.val {
            FieldVal::Const(v) => v.clone(),
            FieldVal::Def(def, idx) => {
                let scope = self.layer_scope(selfv, selfo, i)?;
                let e = &def.fields[*idx].value;
                self.eval(e, &scope)?
            }
        };
        if f.plus {
            if let Some(j) = (0..i).rev().find(|j| layers[*j].field(k).is_some()) {
                let base = self.eval_layer_field(selfv, selfo, layers, j, k)?;
                v = self.binary(BinOp::Add, base, v, 0)?;
            }
        }
        Ok(v)
    }

    /// All field names (hidden included) of a DataSonnet object, sorted.
    pub fn jfields_all(&self, o: &ObjRef) -> Vec<String> {
        let mut out: Vec<String> = match meta_of(o) {
            Some(m) => {
                let b = m.0.borrow();
                let mut ks: Vec<String> = Vec::new();
                for l in b.layers[..b.upto].iter() {
                    for f in &l.fields {
                        if !ks.contains(&f.key) {
                            ks.push(f.key.clone());
                        }
                    }
                }
                ks
            }
            None => o.borrow().props.keys().map(|k| k.to_string()).collect(),
        };
        out.sort();
        out
    }

    /// Whether a DataSonnet object has field `k` (hidden fields count when `all`).
    pub fn jhas(&self, o: &ObjRef, k: &str, all: bool) -> bool {
        if o.borrow().props.contains(k) {
            return true;
        }
        if !all {
            // a super view has no materialized props
            if let Some(m) = meta_of(o) {
                if m.0.borrow().self_obj.is_some() {
                    return self.jfields_all(o).iter().any(|x| x == k);
                }
            }
            return false;
        }
        self.jfields_all(o).iter().any(|x| x == k)
    }

    /// Evaluate a DataSonnet `import` / `importstr` / `importbin`.
    pub fn eval_import(&mut self, kind: u8, path: &str, pos: u32) -> R<V> {
        let Some(file) = resolve_import(path) else {
            return self.throw("RUNTIME ERROR", format!("couldn't open import \"{}\": no match locally or in the library paths", path), Some(pos));
        };
        let key = format!("{}:{}", kind, file.display());
        if let Some(v) = self.imports.get(&key) {
            return Ok(v.clone());
        }
        let data = match std::fs::read(&file) {
            Ok(d) => d,
            Err(e) => return self.throw("RUNTIME ERROR", format!("couldn't open import \"{}\": {}", path, e), Some(pos)),
        };
        let v = match kind {
            1 => V::string(String::from_utf8_lossy(&data).into_owned()),
            2 => V::arr(data.iter().map(|b| V::Int(*b as i64)).collect()),
            _ => {
                let src = String::from_utf8_lossy(&data).into_owned();
                let prog = match crate::jsonnet::parse_program(&src) {
                    Ok(p) => p,
                    Err(e) => {
                        let (l, c) = crate::error::line_col(&src, e.pos);
                        return self.throw("STATIC ERROR", format!("{}:{}:{}: {}", path, l, c, e.msg), Some(pos));
                    }
                };
                let saved = std::mem::replace(&mut self.src, Arc::from(src.as_str()));
                // imported files keep their AST alive for the rest of the evaluation
                let prog = Rc::new(prog);
                self.import_asts.push(prog.clone());
                let env = new_scope(Some(self.globals.clone()), false);
                let mut out = V::Null;
                let mut r = Ok(());
                for s in &prog.body {
                    match self.exec(s, &env) {
                        Ok(v) => out = v,
                        Err(e) => {
                            r = Err(e);
                            break;
                        }
                    }
                }
                self.src = saved;
                r?;
                out
            }
        };
        self.imports.insert(key, v.clone());
        Ok(v)
    }
}

/// Resolve an import path: absolute, relative to the working directory, `classpath:` (searched
/// in the working directory and `src/main/resources` / `src/test/resources`), and the
/// directories of the `DATASONNET_PATH` / `JSONNET_PATH` environment variables.
pub fn resolve_import(path: &str) -> Option<std::path::PathBuf> {
    let p = path.strip_prefix("classpath:").unwrap_or(path).trim_start_matches("classpath:");
    let p = p.strip_prefix("file:").unwrap_or(p);
    let pb = std::path::PathBuf::from(p);
    if pb.is_absolute() {
        return pb.is_file().then_some(pb);
    }
    let rel = p.trim_start_matches('/');
    let mut dirs: Vec<std::path::PathBuf> = vec![std::path::PathBuf::from("."), "src/main/resources".into(), "src/test/resources".into()];
    for var in ["DATASONNET_PATH", "JSONNET_PATH"] {
        if let Ok(v) = std::env::var(var) {
            dirs.extend(v.split(':').filter(|s| !s.is_empty()).map(std::path::PathBuf::from));
        }
    }
    dirs.into_iter().map(|d| d.join(rel)).find(|f| f.is_file())
}
