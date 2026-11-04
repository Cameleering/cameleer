//! Stored routine execution: argument binding, the procedural interpreter and routine
//! results.
//!
//! Routines run against a [`DbCtx`]: mutably (`CALL`, top-level `SELECT fn(constant
//! args)`, `SELECT * FROM fn(constant args)`, nested calls from procedural code) or
//! read-only (functions evaluated per row inside a query, which may only read).
//! Variables and parameters are bound into statements by substituting their values for
//! unqualified column references that do not name a column of a table used by the
//! statement (and `$n` for the n-th parameter).

use std::cell::Cell;
use std::collections::HashSet;
use std::sync::Mutex;

use camel_core::value::{ObjectMap, Value};

use super::ast::*;
use super::eval::{coerce, eval, int_of, text_of, truth, Env, Params, Scope};
use super::exec::{apply_undo, column_type, execute_statement, run_select, Database, Undo};
use super::{SqlErr, SqlResult};
use crate::datasource::{ColumnType, ExecuteOptions, QueryResult};

/// Maximum nesting of routine calls.
pub const MAX_DEPTH: u32 = 48;
/// Stack a routine call must have left, like PostgreSQL's `max_stack_depth` check: nested
/// calls can exhaust a small thread stack (debug builds, Windows) before `MAX_DEPTH`.
const MIN_STACK: usize = 256 * 1024;
/// Maximum iterations of one loop (guards against runaway procedures).
const MAX_ITERATIONS: u64 = 50_000_000;

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Database access for a routine execution.
pub enum DbCtx<'a> {
    Mut(&'a mut Database, &'a mut Vec<Undo>),
    Ref(&'a Database),
}

impl DbCtx<'_> {
    pub fn db(&self) -> &Database {
        match self {
            DbCtx::Mut(d, _) => d,
            DbCtx::Ref(d) => d,
        }
    }

    fn exec(&mut self, stmt: &Statement, now: &str) -> SqlResult<QueryResult> {
        let params = Params { positional: &[], named: None };
        match self {
            DbCtx::Mut(db, undo) => execute_statement(db, stmt, &params, undo, &ExecuteOptions::default(), now),
            DbCtx::Ref(db) => match stmt {
                Statement::Select(s) => {
                    let rel = run_select(db, s, &params, None, now)?;
                    Ok(QueryResult { columns: rel.columns, column_types: rel.types, update_count: rel.rows.len() as i64, rows: rel.rows, is_query: true, command: "SELECT".into(), ..Default::default() })
                }
                _ => Err(SqlErr::new(
                    "25006",
                    "cannot execute a data-modifying statement in a function evaluated per row of a query (call it with constant arguments, from FROM, or use CALL)",
                )),
            },
        }
    }

    fn undo_len(&self) -> usize {
        match self {
            DbCtx::Mut(_, u) => u.len(),
            DbCtx::Ref(_) => 0,
        }
    }

    fn rollback_to(&mut self, mark: usize) {
        if let DbCtx::Mut(db, undo) = self {
            if undo.len() > mark {
                let tail = undo.split_off(mark);
                apply_undo(db, tail);
            }
        }
    }
}

/// The result of a routine call.
#[derive(Clone, Debug, Default)]
pub struct RoutineOutput {
    /// The relation view (`SELECT * FROM fn()` / the OUT-parameter row of `CALL`).
    pub columns: Vec<String>,
    pub types: Vec<ColumnType>,
    pub rows: Vec<Vec<Value>>,
    /// The value in an expression context (`SELECT fn()`).
    pub scalar: Value,
    /// Rows affected by data-modifying statements of the body.
    pub update_count: i64,
    /// A result set produced by a plain SELECT in a procedure (procedures without OUT
    /// parameters return it from `CALL`).
    pub result_set: Option<QueryResult>,
    /// Whether the routine has OUT / INOUT parameters (or `RETURNS record`).
    pub has_out: bool,
}

impl RoutineOutput {
    /// The output of `CALL` as a query result.
    pub fn into_call_result(self) -> QueryResult {
        if self.has_out {
            return QueryResult { columns: self.columns, column_types: self.types, rows: self.rows, update_count: self.update_count, is_query: true, command: "CALL".into(), ..Default::default() };
        }
        if let Some(mut rs) = self.result_set {
            rs.command = "CALL".into();
            return rs;
        }
        QueryResult::update("CALL", self.update_count)
    }
}

static CODES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// A `'static` SQLSTATE for a user-supplied error code.
fn intern(code: &str) -> &'static str {
    let mut g = CODES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = g.iter().find(|c| **c == code) {
        return c;
    }
    let c: &'static str = Box::leak(code.to_string().into_boxed_str());
    g.push(c);
    c
}

/// SQLSTATE of a PL/pgSQL condition name.
pub fn condition_code(name: &str) -> Option<&'static str> {
    Some(match name {
        "raise_exception" => "P0001",
        "no_data_found" => "P0002",
        "too_many_rows" => "P0003",
        "assert_failure" => "P0004",
        "division_by_zero" => "22012",
        "unique_violation" => "23505",
        "not_null_violation" => "23502",
        "foreign_key_violation" => "23503",
        "check_violation" => "23514",
        "invalid_text_representation" => "22P02",
        "numeric_value_out_of_range" => "22003",
        "string_data_right_truncation" => "22001",
        "undefined_table" => "42P01",
        "undefined_column" => "42703",
        "undefined_function" => "42883",
        "syntax_error" => "42601",
        "datatype_mismatch" => "42804",
        _ => return None,
    })
}

fn condition_matches(cond: &str, e: &SqlErr) -> bool {
    if cond == "others" {
        return true;
    }
    if let Some(code) = cond.strip_prefix("sqlstate:") {
        return e.code.eq_ignore_ascii_case(code);
    }
    match cond {
        "data_exception" => e.code.starts_with("22"),
        "integrity_constraint_violation" => e.code.starts_with("23"),
        _ => condition_code(cond).is_some_and(|c| c == e.code),
    }
}

/// Call a routine. `as_call`: `CALL` semantics (arguments may include OUT positions).
pub fn call_routine(ctx: &mut DbCtx, r: &Routine, args: Vec<Value>, as_call: bool, now: &str) -> SqlResult<RoutineOutput> {
    let kind = if r.function { "function" } else { "procedure" };
    let n_in = r.params.iter().filter(|p| p.mode != ParamMode::Out).count();
    let mut vals: Vec<Option<Value>> = vec![None; r.params.len()];
    if as_call && args.len() == r.params.len() && n_in != r.params.len() {
        for (i, v) in args.into_iter().enumerate() {
            if r.params[i].mode != ParamMode::Out {
                vals[i] = Some(v);
            }
        }
    } else {
        if args.len() > n_in {
            return Err(SqlErr::new("42883", format!("{} {}() does not accept {} argument(s) (it has {} input parameter(s))", kind, r.name, args.len(), n_in)));
        }
        let mut it = args.into_iter();
        for (i, p) in r.params.iter().enumerate() {
            if p.mode != ParamMode::Out {
                vals[i] = it.next();
            }
        }
    }
    let mut values = Vec::with_capacity(r.params.len());
    for (i, p) in r.params.iter().enumerate() {
        let v = match (vals[i].take(), p.mode) {
            (_, ParamMode::Out) => Value::Null,
            (Some(v), _) => v,
            (None, _) => match &p.default {
                Some(d) => {
                    let scope = Scope::default();
                    let params = Params { positional: &[], named: None };
                    let env = Env { scope: &scope, row: &[], group: None, params: &params, db: ctx.db(), outer: None, now };
                    eval(d, &env)?
                }
                None => {
                    return Err(SqlErr::new(
                        "42883",
                        format!("{} {}() requires a value for parameter {} ({})", kind, r.name, i + 1, p.name.clone().unwrap_or_else(|| format!("${}", i + 1))),
                    ))
                }
            },
        };
        let v = coerce(v, &p.ty).map_err(|e| SqlErr::new(e.code, format!("{} (parameter {} of {}())", e.message, p.name.clone().unwrap_or_else(|| format!("${}", i + 1)), r.name)))?;
        values.push(v);
    }
    let has_out = r.params.iter().any(|p| p.mode != ParamMode::In);
    if r.strict && r.params.iter().zip(&values).any(|(p, v)| p.mode != ParamMode::Out && v.is_null()) {
        let mut out = RoutineOutput { has_out, ..Default::default() };
        out.columns = vec![r.name.clone()];
        out.types = vec![ColumnType::Unknown];
        out.rows = if matches!(r.returns, Returns::SetOf(..) | Returns::Table(_)) { Vec::new() } else { vec![vec![Value::Null]] };
        return Ok(out);
    }
    let depth = DEPTH.with(|d| d.get());
    if depth >= MAX_DEPTH {
        return Err(SqlErr::new("54001", format!("stack depth limit exceeded: routine calls nested more than {} levels ({}())", MAX_DEPTH, r.name)));
    }
    if camel_core::stack::remaining() < MIN_STACK {
        return Err(SqlErr::new("54001", format!("stack depth limit exceeded: routine calls nested {} levels deep ran out of stack ({}())", depth, r.name)));
    }
    DEPTH.with(|d| d.set(depth + 1));
    let res = run_routine(ctx, r, values, now);
    DEPTH.with(|d| d.set(depth));
    res
}

fn run_routine(ctx: &mut DbCtx, r: &Routine, values: Vec<Value>, now: &str) -> SqlResult<RoutineOutput> {
    let n_params = r.params.len();
    let mut it = Interp {
        ctx,
        now,
        vars: Vec::new(),
        n_params,
        out_rows: Vec::new(),
        set_cols: Vec::new(),
        ret: None,
        result_set: None,
        update_count: 0,
        row_count: 0,
        current_error: None,
        routine: r.name.clone(),
    };
    for (i, (p, v)) in r.params.iter().zip(values).enumerate() {
        it.vars.push(Var { name: p.name.clone().unwrap_or_else(|| format!("${}", i + 1)), ty: Some(p.ty.clone()), value: v, constant: false });
    }
    if let Returns::Table(cols) = &r.returns {
        for (c, ty) in cols {
            it.vars.push(Var { name: c.clone(), ty: Some(ty.clone()), value: Value::Null, constant: false });
            it.set_cols.push(c.clone());
        }
    }
    it.vars.push(Var { name: "found".into(), ty: Some(DataType::Boolean), value: Value::Bool(false), constant: false });
    let mut last: Option<QueryResult> = None;
    match &r.body {
        RoutineBody::Sql(stmts) => {
            for s in stmts {
                let bound = it.bind_stmt(s);
                let res = it.ctx.exec(&bound, now)?;
                if !res.is_query {
                    it.update_count += res.update_count;
                }
                last = Some(res);
            }
        }
        RoutineBody::Pl(block) => match it.run_block(block)? {
            Flow::Normal | Flow::Return => {}
            Flow::Exit(_) | Flow::Continue(_) => return Err(SqlErr::new("42601", "EXIT / CONTINUE cannot be used outside a loop")),
        },
    }
    // build the output
    let mut out = RoutineOutput { update_count: it.update_count, has_out: r.params.iter().any(|p| p.mode != ParamMode::In) || matches!(r.returns, Returns::Record), ..Default::default() };
    let out_params: Vec<(String, Value, DataType)> = r
        .params
        .iter()
        .enumerate()
        .filter(|(_, p)| p.mode != ParamMode::In)
        .map(|(i, p)| (p.name.clone().unwrap_or_else(|| format!("column{}", i + 1)), it.vars[i].value.clone(), p.ty.clone()))
        .collect();
    let pl = matches!(r.body, RoutineBody::Pl(_));
    match &r.returns {
        Returns::Void if !r.function || out_params.is_empty() => {
            if r.function {
                out.columns = vec![r.name.clone()];
                out.types = vec![ColumnType::Unknown];
                out.rows = vec![vec![Value::Null]];
            } else if !out_params.is_empty() {
                let mut vals: Vec<Value> = out_params.iter().map(|(_, v, _)| v.clone()).collect();
                if !pl {
                    // SQL-bodied procedure: the last query's first row supplies the OUT values
                    if let Some(row) = last.as_ref().filter(|l| l.is_query).and_then(|l| l.rows.first()) {
                        for (k, v) in row.iter().enumerate().take(vals.len()) {
                            vals[k] = coerce(v.clone(), &out_params[k].2)?;
                        }
                    }
                }
                out.columns = out_params.iter().map(|(n, _, _)| n.clone()).collect();
                out.types = out_params.iter().map(|(_, _, t)| column_type(t)).collect();
                out.rows = vec![vals];
            }
            if !pl {
                if let Some(l) = last.filter(|l| l.is_query && out_params.is_empty()) {
                    out.result_set = Some(l);
                }
            } else {
                out.result_set = it.result_set.take();
            }
        }
        Returns::Scalar(ty) => {
            let v = if pl {
                match it.ret.take() {
                    Some(v) => v,
                    None => return Err(SqlErr::new("2F005", format!("control reached end of function {}() without RETURN", r.name))),
                }
            } else {
                last.as_ref().and_then(|l| l.scalar().cloned()).unwrap_or(Value::Null)
            };
            let v = coerce(v, ty)?;
            out.columns = vec![r.name.clone()];
            out.types = vec![column_type(ty)];
            out.rows = vec![vec![v.clone()]];
            out.scalar = v;
        }
        Returns::Void | Returns::Record => {
            let mut vals: Vec<Value> = out_params.iter().map(|(_, v, _)| v.clone()).collect();
            if !pl {
                if let Some(row) = last.as_ref().filter(|l| l.is_query).and_then(|l| l.rows.first()) {
                    for (k, v) in row.iter().enumerate().take(vals.len()) {
                        vals[k] = coerce(v.clone(), &out_params[k].2)?;
                    }
                }
            }
            out.columns = out_params.iter().map(|(n, _, _)| n.clone()).collect();
            out.types = out_params.iter().map(|(_, _, t)| column_type(t)).collect();
            out.scalar = if vals.len() == 1 { vals[0].clone() } else { Value::Object(out.columns.iter().cloned().zip(vals.iter().cloned()).collect()) };
            out.rows = vec![vals];
        }
        Returns::SetOf(ty, table) => {
            let (cols, rows) = if pl {
                (it.set_cols.clone(), std::mem::take(&mut it.out_rows))
            } else {
                let l = last.unwrap_or_default();
                (l.columns, l.rows)
            };
            let cols = match (ty, table) {
                (Some(_), _) => vec![r.name.clone()],
                (None, Some(t)) => match it.ctx.db().tables.get(t) {
                    Some(tab) => tab.columns.iter().map(|c| c.name.clone()).collect(),
                    None => cols,
                },
                (None, None) => cols,
            };
            let rows: Vec<Vec<Value>> = match ty {
                Some(t) => rows.into_iter().map(|row| row.into_iter().next().map(|v| coerce(v, t)).transpose().map(|v| vec![v.unwrap_or(Value::Null)])).collect::<SqlResult<_>>()?,
                None => rows,
            };
            out.types = match ty {
                Some(t) => vec![column_type(t)],
                None => cols.iter().enumerate().map(|(i, _)| rows.iter().find_map(|r| r.get(i).filter(|v| !v.is_null()).map(ColumnType::of_value)).unwrap_or(ColumnType::Text)).collect(),
            };
            out.scalar = rows.first().and_then(|r| r.first()).cloned().unwrap_or(Value::Null);
            out.columns = cols;
            out.rows = rows;
        }
        Returns::Table(cols) => {
            let rows = if pl {
                std::mem::take(&mut it.out_rows)
            } else {
                last.map(|l| l.rows).unwrap_or_default()
            };
            let rows: Vec<Vec<Value>> = rows
                .into_iter()
                .map(|row| row.into_iter().zip(cols.iter()).map(|(v, (_, t))| coerce(v, t)).collect::<SqlResult<Vec<_>>>())
                .collect::<SqlResult<_>>()?;
            out.columns = cols.iter().map(|(c, _)| c.clone()).collect();
            out.types = cols.iter().map(|(_, t)| column_type(t)).collect();
            out.scalar = rows.first().and_then(|r| r.first()).cloned().unwrap_or(Value::Null);
            out.rows = rows;
        }
    }
    Ok(out)
}

/// Run an anonymous `DO` block.
pub fn run_do(ctx: &mut DbCtx, block: &Block, now: &str) -> SqlResult<QueryResult> {
    let r = Routine { name: "inline_code_block".into(), function: false, params: Vec::new(), returns: Returns::Void, body: RoutineBody::Pl(block.clone()), strict: false };
    let out = call_routine(ctx, &r, Vec::new(), true, now)?;
    Ok(QueryResult::update("DO", out.update_count))
}

struct Var {
    name: String,
    ty: Option<DataType>,
    value: Value,
    constant: bool,
}

enum Flow {
    Normal,
    Exit(Option<String>),
    Continue(Option<String>),
    Return,
}

struct Interp<'c, 'a> {
    ctx: &'c mut DbCtx<'a>,
    now: &'c str,
    vars: Vec<Var>,
    n_params: usize,
    out_rows: Vec<Vec<Value>>,
    set_cols: Vec<String>,
    ret: Option<Value>,
    result_set: Option<QueryResult>,
    update_count: i64,
    row_count: i64,
    current_error: Option<SqlErr>,
    routine: String,
}

impl Interp<'_, '_> {
    fn var_index(&self, name: &str) -> Option<usize> {
        self.vars.iter().rposition(|v| v.name.eq_ignore_ascii_case(name))
    }

    fn set_found(&mut self, b: bool) {
        if let Some(i) = self.var_index("found") {
            self.vars[i].value = Value::Bool(b);
        }
    }

    fn assign(&mut self, name: &str, field: Option<&str>, v: Value) -> SqlResult<()> {
        let i = self.var_index(name).ok_or_else(|| SqlErr::new("42703", format!("variable \"{}\" does not exist (in {}())", name, self.routine)))?;
        let var = &mut self.vars[i];
        if var.constant {
            return Err(SqlErr::new("22005", format!("variable \"{}\" is declared CONSTANT", name)));
        }
        if let Some(f) = field {
            let mut m = match std::mem::take(&mut var.value) {
                Value::Object(m) => m,
                _ => ObjectMap::new(),
            };
            m.insert(f.to_string(), v);
            var.value = Value::Object(m);
            return Ok(());
        }
        var.value = match &var.ty {
            Some(t) => coerce(v, t)?,
            None => v,
        };
        Ok(())
    }

    /// Substitute variables and `$n` parameters into a statement.
    fn bind_stmt(&self, stmt: &Statement) -> Statement {
        let tables = stmt.table_names();
        let db = self.ctx.db();
        let mut cols: HashSet<String> = HashSet::new();
        for t in &tables {
            if let Some(tab) = db.tables.get(t) {
                cols.extend(tab.columns.iter().map(|c| c.name.to_ascii_lowercase()));
            }
        }
        let mut s = stmt.clone();
        s.walk_mut(&mut |e| {
            let rep = match e {
                Expr::Column { table: None, name } if !cols.contains(&name.to_ascii_lowercase()) => self.var_index(name).map(|i| self.vars[i].value.clone()),
                Expr::Column { table: Some(t), name } if !tables.contains(&t.to_ascii_lowercase()) => match self.var_index(t).map(|i| &self.vars[i].value) {
                    Some(Value::Object(m)) => Some(m.get(&name.to_ascii_lowercase()).or_else(|| m.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v)).cloned().unwrap_or(Value::Null)),
                    _ => None,
                },
                Expr::Param(i) if *i < self.n_params => Some(self.vars[*i].value.clone()),
                _ => None,
            };
            if let Some(v) = rep {
                *e = Expr::Literal(v);
            }
        });
        s
    }

    fn eval_expr(&mut self, e: &Expr) -> SqlResult<Value> {
        let stmt = Statement::Select(Box::new(Select { items: vec![SelectItem::Expr { expr: e.clone(), alias: None }], ..Default::default() }));
        let mut stmt = self.bind_stmt(&stmt);
        let params = Params { positional: &[], named: None };
        if let DbCtx::Mut(db, undo) = &mut *self.ctx {
            super::exec::prepare_routine_calls(db, undo, &mut stmt, &params, self.now)?;
        }
        let Statement::Select(sel) = stmt else { unreachable!() };
        let Some(SelectItem::Expr { expr, .. }) = sel.items.into_iter().next() else { unreachable!() };
        let scope = Scope::default();
        let env = Env { scope: &scope, row: &[], group: None, params: &params, db: self.ctx.db(), outer: None, now: self.now };
        eval(&expr, &env)
    }

    fn cond(&mut self, e: &Expr) -> SqlResult<bool> {
        let v = self.eval_expr(e)?;
        Ok(truth(&v)? == Some(true))
    }

    fn run_block(&mut self, b: &Block) -> SqlResult<Flow> {
        let base = self.vars.len();
        let mark = self.ctx.undo_len();
        let r = self.run_block_body(b);
        let r = match r {
            Err(e) if !b.handlers.is_empty() => match b.handlers.iter().find(|(conds, _)| conds.iter().any(|c| condition_matches(c, &e))) {
                Some((_, stmts)) => {
                    self.ctx.rollback_to(mark);
                    self.vars.push(Var { name: "sqlstate".into(), ty: None, value: Value::String(e.code.to_string()), constant: false });
                    self.vars.push(Var { name: "sqlerrm".into(), ty: None, value: Value::String(e.message.clone()), constant: false });
                    let saved = self.current_error.replace(e);
                    let r = self.run_stmts(stmts);
                    self.current_error = saved;
                    r
                }
                None => Err(e),
            },
            other => other,
        };
        self.vars.truncate(base);
        match r {
            Ok(Flow::Exit(Some(l))) if b.label.as_deref() == Some(l.as_str()) => Ok(Flow::Normal),
            other => other,
        }
    }

    fn run_block_body(&mut self, b: &Block) -> SqlResult<Flow> {
        for d in &b.decls {
            let v = match &d.default {
                Some(e) => self.eval_expr(e)?,
                None => Value::Null,
            };
            let v = match &d.ty {
                Some(t) => coerce(v, t)?,
                None => v,
            };
            if d.not_null && v.is_null() {
                return Err(SqlErr::new("22004", format!("variable \"{}\" declared NOT NULL cannot default to NULL", d.name)));
            }
            self.vars.push(Var { name: d.name.clone(), ty: d.ty.clone(), value: v, constant: d.constant });
        }
        self.run_stmts(&b.body)
    }

    fn run_stmts(&mut self, stmts: &[PlStmt]) -> SqlResult<Flow> {
        for s in stmts {
            match self.run_stmt(s)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    /// Handle the flow of one loop iteration: `Some(flow)` leaves the loop with `flow`.
    fn loop_flow(flow: Flow, label: &Option<String>) -> Option<Flow> {
        match flow {
            Flow::Normal => None,
            Flow::Exit(None) => Some(Flow::Normal),
            Flow::Exit(Some(l)) if label.as_deref() == Some(l.as_str()) => Some(Flow::Normal),
            Flow::Continue(None) => None,
            Flow::Continue(Some(l)) if label.as_deref() == Some(l.as_str()) => None,
            other => Some(other),
        }
    }

    fn run_query(&mut self, stmt: &Statement) -> SqlResult<QueryResult> {
        let bound = self.bind_stmt(stmt);
        self.ctx.exec(&bound, self.now)
    }

    fn run_stmt(&mut self, s: &PlStmt) -> SqlResult<Flow> {
        match s {
            PlStmt::Null => Ok(Flow::Normal),
            PlStmt::Assign { name, field, expr } => {
                let v = self.eval_expr(expr)?;
                self.assign(name, field.as_deref(), v)?;
                Ok(Flow::Normal)
            }
            PlStmt::If { branches, else_ } => {
                for (c, body) in branches {
                    if self.cond(c)? {
                        return self.run_stmts(body);
                    }
                }
                self.run_stmts(else_)
            }
            PlStmt::While { label, cond, body } => {
                let mut n = 0u64;
                while self.cond(cond)? {
                    n += 1;
                    if n > MAX_ITERATIONS {
                        return Err(SqlErr::new("54000", "loop iteration limit exceeded"));
                    }
                    let f = self.run_stmts(body)?;
                    if let Some(f) = Self::loop_flow(f, label) {
                        return Ok(f);
                    }
                }
                Ok(Flow::Normal)
            }
            PlStmt::Loop { label, body } => {
                for _ in 0..MAX_ITERATIONS {
                    let f = self.run_stmts(body)?;
                    if let Some(f) = Self::loop_flow(f, label) {
                        return Ok(f);
                    }
                }
                Err(SqlErr::new("54000", "loop iteration limit exceeded"))
            }
            PlStmt::ForRange { label, var, reverse, from, to, by, body } => {
                let a = int_of(&self.eval_expr(from)?, "FOR loop lower bound")?;
                let b = int_of(&self.eval_expr(to)?, "FOR loop upper bound")?;
                let step = match by {
                    Some(e) => int_of(&self.eval_expr(e)?, "BY")?,
                    None => 1,
                };
                if step <= 0 {
                    return Err(SqlErr::new("22023", "BY value of FOR loop must be greater than zero"));
                }
                let base = self.vars.len();
                self.vars.push(Var { name: var.clone(), ty: Some(DataType::Integer), value: Value::Null, constant: false });
                let mut i = a;
                let mut result = Flow::Normal;
                let mut iterated = false;
                while if *reverse { i >= b } else { i <= b } {
                    iterated = true;
                    let idx = self.vars.len() - 1;
                    self.vars[idx].value = Value::Int(i);
                    let f = match self.run_stmts(body) {
                        Ok(f) => f,
                        Err(e) => {
                            self.vars.truncate(base);
                            return Err(e);
                        }
                    };
                    if let Some(f) = Self::loop_flow(f, label) {
                        result = f;
                        break;
                    }
                    i = if *reverse { i - step } else { i + step };
                }
                self.vars.truncate(base);
                self.set_found(iterated);
                Ok(result)
            }
            PlStmt::ForQuery { label, var, query, body } => {
                let res = self.run_query(query)?;
                let existing = self.var_index(var);
                let base = self.vars.len();
                if existing.is_none() {
                    self.vars.push(Var { name: var.clone(), ty: None, value: Value::Null, constant: false });
                }
                let target = existing.unwrap_or(self.vars.len() - 1);
                let mut result = Flow::Normal;
                let any = !res.rows.is_empty();
                for row in &res.rows {
                    let v = if self.vars[target].ty.is_some() {
                        row.first().cloned().unwrap_or(Value::Null)
                    } else {
                        Value::Object(res.columns.iter().map(|c| c.to_ascii_lowercase()).zip(row.iter().cloned()).collect())
                    };
                    self.vars[target].value = match &self.vars[target].ty {
                        Some(t) => coerce(v, t)?,
                        None => v,
                    };
                    let f = match self.run_stmts(body) {
                        Ok(f) => f,
                        Err(e) => {
                            self.vars.truncate(base);
                            return Err(e);
                        }
                    };
                    if let Some(f) = Self::loop_flow(f, label) {
                        result = f;
                        break;
                    }
                }
                self.vars.truncate(base);
                self.set_found(any);
                Ok(result)
            }
            PlStmt::Exit { label, when } => {
                if let Some(c) = when {
                    if !self.cond(c)? {
                        return Ok(Flow::Normal);
                    }
                }
                Ok(Flow::Exit(label.clone()))
            }
            PlStmt::Continue { label, when } => {
                if let Some(c) = when {
                    if !self.cond(c)? {
                        return Ok(Flow::Normal);
                    }
                }
                Ok(Flow::Continue(label.clone()))
            }
            PlStmt::Return(e) => {
                if let Some(e) = e {
                    self.ret = Some(self.eval_expr(e)?);
                }
                Ok(Flow::Return)
            }
            PlStmt::ReturnNext(e) => {
                match e {
                    Some(e) => {
                        let v = self.eval_expr(e)?;
                        match v {
                            Value::Object(m) => {
                                if self.set_cols.is_empty() {
                                    self.set_cols = m.keys().cloned().collect();
                                }
                                self.out_rows.push(m.values().cloned().collect());
                            }
                            other => self.out_rows.push(vec![other]),
                        }
                    }
                    None => {
                        let row: Vec<Value> = self.set_cols.iter().map(|c| self.var_index(c).map(|i| self.vars[i].value.clone()).unwrap_or(Value::Null)).collect();
                        self.out_rows.push(row);
                    }
                }
                Ok(Flow::Normal)
            }
            PlStmt::ReturnQuery(q) => {
                let res = self.run_query(q)?;
                if self.set_cols.is_empty() {
                    self.set_cols = res.columns.clone();
                }
                self.row_count = res.rows.len() as i64;
                self.out_rows.extend(res.rows);
                Ok(Flow::Normal)
            }
            PlStmt::Raise { level, format, args, errcode } => {
                let Some(level) = level else {
                    return match &self.current_error {
                        Some(e) => Err(e.clone()),
                        None => Err(SqlErr::new("0Z002", "RAISE without parameters cannot be used outside an exception handler")),
                    };
                };
                let mut vals = Vec::new();
                for a in args {
                    vals.push(self.eval_expr(a)?);
                }
                let mut msg = String::new();
                let mut it = vals.iter();
                let mut chars = format.chars().peekable();
                while let Some(c) = chars.next() {
                    if c == '%' {
                        if chars.peek() == Some(&'%') {
                            chars.next();
                            msg.push('%');
                        } else {
                            msg.push_str(&it.next().map(|v| if v.is_null() { "<NULL>".to_string() } else { text_of(v) }).unwrap_or_default());
                        }
                    } else {
                        msg.push(c);
                    }
                }
                if level == "EXCEPTION" {
                    let code = errcode.as_deref().map(intern).unwrap_or("P0001");
                    return Err(SqlErr::new(code, msg));
                }
                match level.as_str() {
                    "DEBUG" | "LOG" => camel_core::camel_debug!("rustsql", "{}(): {}", self.routine, msg),
                    _ => camel_core::camel_info!("rustsql", "{} from {}(): {}", level, self.routine, msg),
                }
                Ok(Flow::Normal)
            }
            PlStmt::Sql { stmt, into } => {
                let res = self.run_query(stmt)?;
                let n = if res.is_query { res.rows.len() as i64 } else { res.update_count };
                self.row_count = n;
                if !res.is_query || !matches!(**stmt, Statement::Select(_)) {
                    self.update_count += res.update_count;
                }
                self.set_found(n > 0);
                match into {
                    Some(t) => {
                        if t.strict && res.rows.is_empty() {
                            return Err(SqlErr::new("P0002", "query returned no rows"));
                        }
                        if t.strict && res.rows.len() > 1 {
                            return Err(SqlErr::new("P0003", "query returned more than one row"));
                        }
                        let row = res.rows.first().cloned().unwrap_or_else(|| vec![Value::Null; res.columns.len()]);
                        let record_target = t.vars.len() == 1 && self.var_index(&t.vars[0]).is_some_and(|i| self.vars[i].ty.is_none());
                        if record_target {
                            let m: ObjectMap = res.columns.iter().map(|c| c.to_ascii_lowercase()).zip(row).collect();
                            self.assign(&t.vars[0], None, Value::Object(m))?;
                        } else {
                            for (k, var) in t.vars.iter().enumerate() {
                                self.assign(var, None, row.get(k).cloned().unwrap_or(Value::Null))?;
                            }
                        }
                    }
                    None => {
                        if res.is_query && matches!(**stmt, Statement::Select(_)) {
                            self.result_set = Some(res);
                        }
                    }
                }
                Ok(Flow::Normal)
            }
            PlStmt::Perform(q) => {
                let res = self.run_query(q)?;
                self.row_count = res.rows.len() as i64;
                self.set_found(!res.rows.is_empty());
                Ok(Flow::Normal)
            }
            PlStmt::GetDiagnostics(items) => {
                for (v, item) in items {
                    let val = match item.as_str() {
                        "ROW_COUNT" => Value::Int(self.row_count),
                        other => return Err(SqlErr::new("0A000", format!("GET DIAGNOSTICS {} is not supported", other))),
                    };
                    self.assign(v, None, val)?;
                }
                Ok(Flow::Normal)
            }
            PlStmt::Block(b) => self.run_block(b),
        }
    }
}
