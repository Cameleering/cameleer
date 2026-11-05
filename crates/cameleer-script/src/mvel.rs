//! MVEL extras: interceptors (`@Name statement` with before / after hooks registered from
//! Rust) and MVEL 2 templates (orb tags) with the `mvel:` template component.
//!
//! # Interceptors
//!
//! An MVEL statement prefixed with `@Name` runs the interceptor registered under `Name`:
//! its `before` hook sees (and may change) the script variables before the statement runs,
//! its `after` hook sees the statement's value and may replace it. Interceptors are looked up
//! in the Camel registry (a [`MvelInterceptor`] bound under the name) and then in the
//! process-wide registry of [`register_interceptor`].
//!
//! ```text
//! @Audit total = price * qty;   // Audit.before(vars), then Audit.after(vars, total)
//! ```
//!
//! # Templates
//!
//! [`render_template`] / the `mvel:` component render MVEL 2 templates: `@{expr}`,
//! `@if{cond}..@elseif{cond}..@else{}..@end{}`, `@foreach{x : list}..@end{', '}` (an optional
//! separator expression in `@end{}`, several iterables `@foreach{a : as, b : bs}`),
//! `@code{statements}` (silent), `@comment{..}`, `@include{'file'}`,
//! `@declare{'name'}..@end{}` + `@includeNamed{'name'}`, and `@@` for a literal `@`.
//! The data model is Camel's (`body`, `headers`, `exchange`, `request`/`in`, `variables`).

use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use cameleer_core::component::template_support::{PartialLoader, TemplateEndpoint, TemplateEngine, TemplateHeaders, TemplateOptions};
use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Headers;
use cameleer_core::spi::{Component, Endpoint};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;

use crate::ast::*;
use crate::error::SyntaxError;
use crate::interp::{Ctrl, Env, Interp, R};
use crate::value::{from_value, to_value, V};

// ====================================================================== interceptors

/// The variables an interceptor works on (changes are written back to the script).
pub struct InterceptContext {
    /// The interceptor name (`@Name`).
    pub name: String,
    /// Script variables visible at the intercepted statement.
    pub variables: BTreeMap<String, Value>,
}

type BeforeFn = dyn Fn(&mut InterceptContext) -> Result<()> + Send + Sync;
type AfterFn = dyn Fn(&mut InterceptContext, Value) -> Result<Value> + Send + Sync;

/// An MVEL interceptor (MVEL's `Interceptor.doBefore` / `doAfter`).
#[derive(Clone, Default)]
pub struct MvelInterceptor {
    before: Option<Arc<BeforeFn>>,
    after: Option<Arc<AfterFn>>,
}

impl MvelInterceptor {
    pub fn new() -> MvelInterceptor {
        MvelInterceptor::default()
    }
    /// Run before the statement; may read and change the script variables.
    pub fn before(mut self, f: impl Fn(&mut InterceptContext) -> Result<()> + Send + Sync + 'static) -> Self {
        self.before = Some(Arc::new(f));
        self
    }
    /// Run after the statement with its value; returns the (possibly replaced) value.
    pub fn after(mut self, f: impl Fn(&mut InterceptContext, Value) -> Result<Value> + Send + Sync + 'static) -> Self {
        self.after = Some(Arc::new(f));
        self
    }
}

fn global_interceptors() -> &'static Mutex<HashMap<String, MvelInterceptor>> {
    static REG: OnceLock<Mutex<HashMap<String, MvelInterceptor>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register an interceptor process-wide (used when the Camel registry has none of that name).
pub fn register_interceptor(name: impl Into<String>, interceptor: MvelInterceptor) {
    global_interceptors().lock().unwrap().insert(name.into(), interceptor);
}

/// Remove a process-wide interceptor.
pub fn unregister_interceptor(name: &str) {
    global_interceptors().lock().unwrap().remove(name);
}

fn scope_vars(env: &Env) -> BTreeMap<String, V> {
    let mut out = BTreeMap::new();
    let mut s = Some(env.clone());
    let mut chain = vec![];
    while let Some(sc) = s {
        chain.push(sc.clone());
        s = sc.parent.clone();
    }
    // outer scopes first, inner ones override
    for sc in chain.iter().rev() {
        for (k, v) in sc.vars.borrow().iter() {
            if !k.starts_with('%') && !matches!(v, V::Host(_) | V::Func(_) | V::Method(_)) {
                out.insert(k.to_string(), v.clone());
            }
        }
    }
    out
}

impl Interp {
    fn find_interceptor(&self, name: &str) -> Option<MvelInterceptor> {
        if let Some(ctx) = &self.host.context {
            if let Some(i) = ctx.registry().lookup::<MvelInterceptor>(name) {
                return Some((*i).clone());
            }
        }
        global_interceptors().lock().unwrap().get(name).cloned()
    }

    /// `@Name statement`: run the interceptor hooks around the statement.
    pub fn mvel_intercept(&mut self, name: &str, s: &Stmt, env: &Env) -> R<V> {
        let Some(icp) = self.find_interceptor(name) else {
            return self.throw("CompileException", format!("no interceptor named '{}' is registered", name), None);
        };
        let mut ctx = InterceptContext { name: name.to_string(), variables: BTreeMap::new() };
        if let Some(b) = &icp.before {
            let vars = scope_vars(env);
            ctx.variables = vars.iter().map(|(k, v)| (k.clone(), to_value(v))).collect();
            let before = ctx.variables.clone();
            if let Err(e) = b(&mut ctx) {
                return self.throw("InterceptorException", e.message, None);
            }
            self.write_back(&before, &ctx.variables, env);
        }
        let v = self.exec(s, env)?;
        let Some(a) = &icp.after else { return Ok(v) };
        let vars = scope_vars(env);
        ctx.variables = vars.iter().map(|(k, v)| (k.clone(), to_value(v))).collect();
        let before = ctx.variables.clone();
        let r = match a(&mut ctx, to_value(&v)) {
            Ok(r) => r,
            Err(e) => return self.throw("InterceptorException", e.message, None),
        };
        self.write_back(&before, &ctx.variables, env);
        Ok(from_value(&r))
    }

    fn write_back(&mut self, before: &BTreeMap<String, Value>, after: &BTreeMap<String, Value>, env: &Env) {
        // new variables go to the enclosing function / script scope
        let mut top = env.clone();
        while !top.func {
            match top.parent.clone() {
                Some(p) => top = p,
                None => break,
            }
        }
        for (k, v) in after {
            if before.get(k) != Some(v) && !env.assign(k, from_value(v)) {
                top.define(k, from_value(v));
            }
        }
    }
}

// ====================================================================== templates

/// Compile an MVEL template into a program that appends to `%out`.
pub fn compile_template(src: &str) -> std::result::Result<Program, SyntaxError> {
    compile_template_with(src, None, 0)
}

fn compile_template_with(src: &str, loader: Option<PartialLoader<'_>>, depth: usize) -> std::result::Result<Program, SyntaxError> {
    let mut c = TplCompiler { src, i: 0, declared: HashMap::new(), tmp: 0, loader, depth };
    let (body, end) = c.block(&[])?;
    if let Some((tag, at)) = end {
        return Err(SyntaxError { pos: at, msg: format!("unexpected @{}{{}} in template", tag) });
    }
    let mut stmts = body;
    stmts.push(Stmt::Expr(Expr::Ident("%out".into(), 0)));
    Ok(Program { dialect: Dialect::Mvel, body: stmts })
}

struct TplCompiler<'a> {
    src: &'a str,
    i: usize,
    declared: HashMap<String, Vec<Stmt>>,
    tmp: usize,
    /// Resolves `@include{'literal'}` relative to the template (inlined at compile time).
    loader: Option<PartialLoader<'a>>,
    depth: usize,
}

fn append(e: Expr) -> Stmt {
    Stmt::Expr(Expr::Assign { op: Some(AssignOp::Bin(BinOp::Add)), target: Box::new(Pat::Id("%out".into())), value: Box::new(Expr::Call { callee: Box::new(Expr::Ident("%tplstr".into(), 0)), args: vec![Elem::Item(e)], optional: false, pos: 0 }), pos: 0 })
}

impl<'a> TplCompiler<'a> {
    fn err<X>(&self, at: usize, msg: impl Into<String>) -> std::result::Result<X, SyntaxError> {
        Err(SyntaxError { pos: at, msg: msg.into() })
    }

    /// The `{...}` content after an orb tag name (at `self.i` = the `{`).
    fn braces(&mut self) -> std::result::Result<(String, usize), SyntaxError> {
        let start = self.i;
        if self.src.as_bytes().get(self.i) != Some(&b'{') {
            return self.err(start, "expected '{' after the orb tag");
        }
        let end = crate::lexer::matching_brace(self.src, self.i + 1).ok_or(SyntaxError { pos: start, msg: "unterminated orb tag".into() })?;
        let inner = self.src[self.i + 1..end].to_string();
        self.i = end + 1;
        Ok((inner, start + 1))
    }

    fn expr(&self, code: &str, off: usize) -> std::result::Result<Expr, SyntaxError> {
        crate::parser::parse_embedded(code, off, Dialect::Mvel)
    }

    /// Compile until one of the closing tags (`end`, `else`, `elseif`); returns the statements
    /// and the closing tag with its content offset.
    #[allow(clippy::type_complexity)]
    fn block(&mut self, closers: &[&str]) -> std::result::Result<(Vec<Stmt>, Option<(String, usize)>), SyntaxError> {
        let mut out = Vec::new();
        let mut lit = String::new();
        let b = self.src.as_bytes();
        while self.i < b.len() {
            if b[self.i] != b'@' {
                let ch = self.src[self.i..].chars().next().unwrap();
                lit.push(ch);
                self.i += ch.len_utf8();
                continue;
            }
            if b.get(self.i + 1) == Some(&b'@') {
                lit.push('@');
                self.i += 2;
                continue;
            }
            // tag name
            let mut j = self.i + 1;
            while j < b.len() && b[j].is_ascii_alphabetic() {
                j += 1;
            }
            let tag = self.src[self.i + 1..j].to_string();
            if b.get(j) != Some(&b'{') {
                lit.push('@');
                self.i += 1;
                continue;
            }
            if !lit.is_empty() {
                out.push(append(Expr::Str(std::mem::take(&mut lit))));
            }
            let tag_at = self.i;
            self.i = j;
            match tag.as_str() {
                "" => {
                    let (code, off) = self.braces()?;
                    out.push(append(self.expr(&code, off)?));
                }
                t if closers.contains(&t) => return Ok((out, Some((tag, tag_at)))),
                "end" | "else" | "elseif" => return self.err(tag_at, format!("unexpected @{}{{}}", tag)),
                "comment" => {
                    self.braces()?;
                }
                "code" => {
                    let (code, off) = self.braces()?;
                    let p = crate::parser::parse_program(&code, Dialect::Mvel).map_err(|e| SyntaxError { pos: e.pos + off, msg: e.msg })?;
                    out.extend(p.body);
                }
                "if" => out.push(self.if_tag()?),
                "foreach" => out.extend(self.foreach_tag()?),
                "include" => {
                    let (code, off) = self.braces()?;
                    let e = self.expr(&code, off)?;
                    if let (Expr::Str(name), Some(load)) = (&e, self.loader) {
                        if let Some(text) = load(name) {
                            if self.depth > 16 {
                                return self.err(off, "templates included too deeply");
                            }
                            let p = compile_template_with(&text, self.loader, self.depth + 1).map_err(|e| SyntaxError { pos: off, msg: format!("{}: {}", name, e.msg) })?;
                            // the included program ends with its `%out` value: drop it
                            let mut body = p.body;
                            body.pop();
                            out.extend(body);
                            continue;
                        }
                    }
                    out.push(append(Expr::Call { callee: Box::new(Expr::Ident("%tplinclude".into(), off as u32)), args: vec![Elem::Item(e)], optional: false, pos: off as u32 }));
                }
                "declare" => {
                    let (code, off) = self.braces()?;
                    let name = match self.expr(&code, off)? {
                        Expr::Str(s) => s,
                        _ => return self.err(off, "@declare{} needs a literal template name"),
                    };
                    let (body, end) = self.block(&["end"])?;
                    if end.is_none() {
                        return self.err(tag_at, "@declare{} without @end{}");
                    }
                    self.braces()?;
                    self.declared.insert(name, body);
                }
                "includeNamed" => {
                    let (code, off) = self.braces()?;
                    let name = match self.expr(&code, off)? {
                        Expr::Str(s) => s,
                        _ => return self.err(off, "@includeNamed{} needs a literal template name"),
                    };
                    match self.declared.get(&name) {
                        Some(b) => out.extend(b.clone()),
                        None => return self.err(off, format!("no template declared as '{}'", name)),
                    }
                }
                other => return self.err(tag_at, format!("unknown orb tag @{}{{}}", other)),
            }
        }
        if !lit.is_empty() {
            out.push(append(Expr::Str(lit)));
        }
        Ok((out, None))
    }

    fn if_tag(&mut self) -> std::result::Result<Stmt, SyntaxError> {
        let start = self.i;
        let (code, off) = self.braces()?;
        let cond = self.expr(&code, off)?;
        let (body, end) = self.block(&["end", "else", "elseif"])?;
        let Some((tag, _)) = end else { return self.err(start, "@if{} without @end{}") };
        let els = match tag.as_str() {
            "end" => {
                self.braces()?;
                None
            }
            "elseif" => Some(Box::new(self.if_tag()?)),
            _ => {
                let (c, o) = self.braces()?;
                if !c.trim().is_empty() {
                    // `@else{cond}` acts as `@elseif{cond}` in MVEL
                    let cond2 = self.expr(&c, o)?;
                    let (b2, e2) = self.block(&["end", "else", "elseif"])?;
                    let Some((t2, _)) = e2 else { return self.err(start, "@else{} without @end{}") };
                    if t2 != "end" {
                        return self.err(start, "only one @else{} branch is supported after a conditional @else{cond}");
                    }
                    self.braces()?;
                    Some(Box::new(Stmt::If(cond2, Box::new(Stmt::Block(b2)), None)))
                } else {
                    let (b2, e2) = self.block(&["end"])?;
                    if e2.is_none() {
                        return self.err(start, "@else{} without @end{}");
                    }
                    self.braces()?;
                    Some(Box::new(Stmt::Block(b2)))
                }
            }
        };
        Ok(Stmt::If(cond, Box::new(Stmt::Block(body)), els))
    }

    fn foreach_tag(&mut self) -> std::result::Result<Vec<Stmt>, SyntaxError> {
        let start = self.i;
        let (code, off) = self.braces()?;
        // `x : list, y : list2`
        let mut iters = Vec::new();
        for part in split_top(&code, ',') {
            let (name, expr) = match part.0.split_once(':') {
                Some((n, e)) => (n.trim().to_string(), (e.to_string(), off + part.1 + n.len() + 1)),
                None => {
                    // `@foreach{list}`: elements bound to `item`? MVEL requires a name
                    return self.err(off, "expected 'name : iterable' in @foreach{}");
                }
            };
            iters.push((name, self.expr(&expr.0, expr.1)?));
        }
        let (body, end) = self.block(&["end"])?;
        if end.is_none() {
            return self.err(start, "@foreach{} without @end{}");
        }
        let (sep_code, sep_off) = self.braces()?;
        let sep = if sep_code.trim().is_empty() { None } else { Some(self.expr(&sep_code, sep_off)?) };
        self.tmp += 1;
        let idx = format!("%fi{}", self.tmp);
        let lists = format!("%fl{}", self.tmp);
        let id = |n: &str| Expr::Ident(n.to_string(), 0);
        let mut out = vec![Stmt::Expr(Expr::Assign { op: None, target: Box::new(Pat::Id(lists.clone())), value: Box::new(Expr::Array(iters.iter().map(|(_, e)| Elem::Item(Expr::Call { callee: Box::new(id("%tpllist")), args: vec![Elem::Item(e.clone())], optional: false, pos: 0 })).collect())), pos: 0 })];
        out.push(Stmt::Expr(Expr::Assign { op: None, target: Box::new(Pat::Id(idx.clone())), value: Box::new(Expr::Int(0)), pos: 0 }));
        // iterate up to the length of the first list
        let first_len = Expr::Call { callee: Box::new(id("%tpllen")), args: vec![Elem::Item(Expr::Index { obj: Box::new(id(&lists)), index: Box::new(Expr::Int(0)), optional: false, pos: 0 })], optional: false, pos: 0 };
        let mut loop_body = Vec::new();
        if let Some(sep) = sep {
            loop_body.push(Stmt::If(Expr::Bin(BinOp::Gt, Box::new(id(&idx)), Box::new(Expr::Int(0)), 0), Box::new(append(sep)), None));
        }
        for (k, (name, _)) in iters.iter().enumerate() {
            let item = Expr::Index { obj: Box::new(Expr::Index { obj: Box::new(id(&lists)), index: Box::new(Expr::Int(k as i64)), optional: false, pos: 0 }), index: Box::new(id(&idx)), optional: false, pos: 0 };
            loop_body.push(Stmt::Expr(Expr::Assign { op: None, target: Box::new(Pat::Id(name.clone())), value: Box::new(item), pos: 0 }));
        }
        loop_body.extend(body);
        out.push(Stmt::For {
            init: None,
            test: Some(Expr::Bin(BinOp::Lt, Box::new(id(&idx)), Box::new(first_len), 0)),
            update: Some(Expr::Update { inc: true, prefix: false, target: Box::new(id(&idx)), pos: 0 }),
            body: Box::new(Stmt::Block(loop_body)),
        });
        Ok(out)
    }
}

/// Split at top-level `sep` (outside brackets and strings); returns (part, offset).
fn split_top(s: &str, sep: char) -> Vec<(String, usize)> {
    let mut out = vec![];
    let mut depth = 0;
    let mut start = 0;
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push((s[start..i].to_string(), start));
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push((s[start..].to_string(), start));
    out
}

impl Interp {
    /// Template helpers: `%tplstr` (MVEL string conversion), `%tpllist` (iterables as lists),
    /// `%tplinclude` (render an included template file with the same variables).
    pub fn mvel_template_fn(&mut self, name: &str, args: &[V], pos: u32) -> R<Option<V>> {
        let a0 = args.first().cloned().unwrap_or(V::Null);
        Ok(Some(match name {
            "%tplstr" => V::string(self.display(&a0)),
            "%tpllen" => V::Int(match &a0 {
                V::Arr(a) => a.borrow().len() as i64,
                _ => 0,
            }),
            "%tpllist" => match &a0 {
                V::Null | V::Undef => V::arr(vec![]),
                other => V::arr(self.iterate(other, Some(pos))?),
            },
            "%tplinclude" => {
                let path = self.display(&a0);
                if self.module_dirs.len() > 16 {
                    return self.throw("TemplateRuntimeError", "templates included too deeply".into(), Some(pos));
                }
                let base = self.module_dirs.last().cloned().or_else(|| self.script_dir.clone());
                let file = match &base {
                    Some(d) if !path.contains(':') && !path.starts_with('/') && d.join(&path).is_file() => d.join(&path),
                    _ => cameleer_core::component::template_support::resource_path(&path),
                };
                let src = match std::fs::read_to_string(&file) {
                    Ok(s) => s,
                    Err(e) => return self.throw("TemplateRuntimeError", format!("cannot include '{}': {}", path, e), Some(pos)),
                };
                let prog = match compile_template(&src) {
                    Ok(p) => p,
                    Err(e) => return self.throw("CompileException", format!("{}: {}", path, e.msg), Some(pos)),
                };
                let prog = std::rc::Rc::new(prog);
                self.import_asts.push(prog.clone());
                let env = crate::interp::new_scope(Some(self.globals.clone()), true);
                env.define("%out", V::str(""));
                self.module_dirs.push(file.parent().map(|p| p.to_path_buf()).unwrap_or_default());
                let mut r = Ok(V::Null);
                for s in &prog.body {
                    match self.exec(s, &env) {
                        Ok(v) => r = Ok(v),
                        Err(Ctrl::Throw(e)) => {
                            r = Err(Ctrl::Throw(e));
                            break;
                        }
                        Err(_) => {}
                    }
                }
                self.module_dirs.pop();
                r?
            }
            _ => return Ok(None),
        }))
    }
}

/// Render an MVEL template against a data model (a map of variables).
pub fn render_template(template: &str, model: &Value) -> Result<String> {
    render_template_with(template, model, None)
}

/// Render with a loader resolving literal `@include{'name'}`s (relative to the template).
pub fn render_template_with(template: &str, model: &Value, loader: Option<PartialLoader<'_>>) -> Result<String> {
    let prog = compile_template_with(template, loader, 0).map_err(|e| e.into_camel(Dialect::Mvel, template))?;
    let get = |k: &str| match model {
        Value::Object(m) => m.get(k).cloned(),
        _ => None,
    };
    let body = get("body").unwrap_or(Value::Null);
    let mut headers = Headers::new();
    if let Some(Value::Object(h)) = get("headers") {
        for (k, v) in h {
            headers.set(k.clone(), v.clone());
        }
    }
    let props: BTreeMap<String, Value> = match get("exchange").as_ref().and_then(|e| match e {
        Value::Object(m) => m.get("properties").cloned(),
        _ => None,
    }) {
        Some(Value::Object(p)) => p.into_iter().collect(),
        _ => BTreeMap::new(),
    };
    let prev = crate::arena::enter();
    let state = crate::host::HostState::new(String::new(), "InOnly".into(), String::new(), None, headers, body, props, None, None);
    let host = std::rc::Rc::new(state);
    let mut it = Interp::new(Dialect::Mvel, Arc::from(template), host);
    it.globals.define("%out", V::str(""));
    for f in ["%tplstr", "%tpllist", "%tpllen", "%tplinclude"] {
        it.globals.define(f, V::Method(std::rc::Rc::new((V::Null, f.to_string()))));
    }
    if let Value::Object(m) = model {
        for (k, v) in m {
            // keep the Java-like exchange / message bindings; add the other model entries
            if !matches!(k.as_str(), "exchange" | "request" | "headers" | "body") {
                it.globals.define(k, from_value(v));
            }
        }
    }
    let r = it.run_program(&prog);
    let out = match r {
        Ok(v) => Ok(it.display(&v)),
        Err(Ctrl::Throw(e)) => Err(CamelError::new(cameleer_core::error::kinds::EXPRESSION_EVALUATION, format!("MVEL template error: {}", it.display(&e)))),
        Err(_) => Ok(String::new()),
    };
    it.teardown();
    drop(it);
    crate::arena::exit(prev);
    out
}

/// The MVEL template engine of the `mvel:` component.
pub struct MvelTemplateEngine;

impl TemplateEngine for MvelTemplateEngine {
    fn render(&self, template: &str, context: &Value, partials: PartialLoader<'_>) -> Result<String> {
        render_template_with(template, context, Some(partials))
    }
}

/// The `mvel:` component: `mvel:template.mvel` renders an MVEL template (headers
/// `CamelMvelTemplate` / `CamelMvelResourceUri` with `allowTemplateFromHeader`, `contentCache`,
/// `allowContextMapAll`).
#[derive(Default)]
pub struct MvelComponent;

impl Component for MvelComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let options = TemplateOptions::from_params(params)?;
        let headers = TemplateHeaders { template: "CamelMvelTemplate", resource_uri: "CamelMvelResourceUri", data_model: None, supplemental: None };
        Ok(Arc::new(TemplateEndpoint::new(ctx, uri, remaining, options, headers, Arc::new(MvelTemplateEngine))?))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
