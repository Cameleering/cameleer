//! Storage and statement execution.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use camel_core::value::Value;

use super::ast::*;
use super::eval::{coerce, compare, eval, group_key, int_of, truth, Env, Params, Scope, ScopeCol};
use super::{SqlErr, SqlResult};
use crate::datasource::{ColumnType, ExecuteOptions, QueryResult};

/// A table column.
#[derive(Clone, Debug)]
pub struct Column {
    pub name: String,
    pub ty: DataType,
    pub not_null: bool,
    pub default: Option<Expr>,
    pub auto_increment: bool,
}

/// A stored row (with a stable internal row id).
#[derive(Clone, Debug)]
pub struct Row {
    pub id: u64,
    pub values: Vec<Value>,
}

/// A table.
#[derive(Clone, Debug)]
pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
    /// Unique constraints: column indexes, constraint name.
    pub uniques: Vec<(Vec<usize>, String)>,
    next_rowid: u64,
    serial: i64,
}

impl Table {
    fn col_index(&self, name: &str) -> SqlResult<usize> {
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| SqlErr::new("42703", format!("column \"{}\" of relation \"{}\" does not exist", name, self.name)))
    }

    fn scope(&self, alias: Option<&str>) -> Scope {
        let t = alias.unwrap_or(&self.name).to_string();
        Scope { cols: self.columns.iter().map(|c| ScopeCol { table: Some(t.clone()), name: c.name.clone(), ty: Some(c.ty.clone()) }).collect() }
    }

    fn position_of(&self, rowid: u64) -> Option<usize> {
        self.rows.binary_search_by_key(&rowid, |r| r.id).ok()
    }

    /// Validate NOT NULL and unique constraints over the whole table.
    fn validate(&self) -> SqlResult<()> {
        for (cols, cname) in &self.uniques {
            let mut seen = HashSet::new();
            for r in &self.rows {
                let key: Vec<Value> = cols.iter().map(|i| r.values[*i].clone()).collect();
                if key.iter().any(|v| v.is_null()) {
                    continue;
                }
                if !seen.insert(group_key(&key)) {
                    let names: Vec<&str> = cols.iter().map(|i| self.columns[*i].name.as_str()).collect();
                    let vals: Vec<String> = key.iter().map(super::eval::text_of).collect();
                    return Err(SqlErr::new(
                        "23505",
                        format!("duplicate key value violates unique constraint \"{}\": Key ({})=({}) already exists", cname, names.join(", "), vals.join(", ")),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The in-memory database (a set of tables).
#[derive(Clone, Debug, Default)]
pub struct Database {
    pub tables: BTreeMap<String, Table>,
}

impl Database {
    fn table(&self, name: &str) -> SqlResult<&Table> {
        self.tables.get(&name.to_ascii_lowercase()).ok_or_else(|| SqlErr::new("42P01", format!("relation \"{}\" does not exist", name)))
    }
    fn table_mut(&mut self, name: &str) -> SqlResult<&mut Table> {
        self.tables.get_mut(&name.to_ascii_lowercase()).ok_or_else(|| SqlErr::new("42P01", format!("relation \"{}\" does not exist", name)))
    }
    /// Table names (as created).
    pub fn table_names(&self) -> Vec<String> {
        self.tables.values().map(|t| t.name.clone()).collect()
    }
}

/// An undo record for transaction / statement rollback.
#[derive(Debug)]
pub enum Undo {
    Insert { table: String, rowid: u64 },
    Update { table: String, rowid: u64, old: Vec<Value> },
    Delete { table: String, row: Row },
    CreateTable { table: String },
    DropTable { table: Table },
    Truncate { table: String, rows: Vec<Row> },
}

/// Apply undo records (newest first).
pub fn apply_undo(db: &mut Database, undo: Vec<Undo>) {
    for u in undo.into_iter().rev() {
        match u {
            Undo::Insert { table, rowid } => {
                if let Some(t) = db.tables.get_mut(&table) {
                    if let Some(p) = t.position_of(rowid) {
                        t.rows.remove(p);
                    }
                }
            }
            Undo::Update { table, rowid, old } => {
                if let Some(t) = db.tables.get_mut(&table) {
                    if let Some(p) = t.position_of(rowid) {
                        t.rows[p].values = old;
                    }
                }
            }
            Undo::Delete { table, row } => {
                if let Some(t) = db.tables.get_mut(&table) {
                    let p = t.rows.binary_search_by_key(&row.id, |r| r.id).unwrap_or_else(|p| p);
                    t.rows.insert(p, row);
                }
            }
            Undo::CreateTable { table } => {
                db.tables.remove(&table);
            }
            Undo::DropTable { table } => {
                db.tables.insert(table.name.to_ascii_lowercase(), table);
            }
            Undo::Truncate { table, rows } => {
                if let Some(t) = db.tables.get_mut(&table) {
                    t.rows = rows;
                }
            }
        }
    }
}

/// A relation (intermediate or final query result).
pub struct Relation {
    pub columns: Vec<String>,
    pub types: Vec<ColumnType>,
    pub rows: Vec<Vec<Value>>,
}

fn column_type(ty: &DataType) -> ColumnType {
    match ty {
        DataType::Integer => ColumnType::Integer,
        DataType::BigInt => ColumnType::BigInt,
        DataType::Varchar(_) | DataType::Text => ColumnType::Text,
        DataType::Boolean => ColumnType::Boolean,
        DataType::Double => ColumnType::Double,
        DataType::Decimal(..) => ColumnType::Decimal,
        DataType::Timestamp | DataType::Date => ColumnType::Timestamp,
        DataType::Bytes => ColumnType::Bytes,
    }
}

/// Execute a non-transaction-control statement.
pub fn execute_statement(db: &mut Database, stmt: &Statement, params: &Params, undo: &mut Vec<Undo>, opts: &ExecuteOptions, now: &str) -> SqlResult<QueryResult> {
    match stmt {
        Statement::Select(sel) => {
            let mut rel = run_select(db, sel, params, None, now)?;
            if opts.max_rows > 0 {
                rel.rows.truncate(opts.max_rows);
            }
            let n = rel.rows.len() as i64;
            Ok(QueryResult { columns: rel.columns, column_types: rel.types, rows: rel.rows, update_count: n, is_query: true, command: "SELECT".into(), ..Default::default() })
        }
        Statement::Insert { table, columns, source, returning } => exec_insert(db, table, columns.as_deref(), source, returning.as_deref(), params, undo, now),
        Statement::Update { table, alias, sets, where_, returning } => exec_update(db, table, alias.as_deref(), sets, where_.as_ref(), returning.as_deref(), params, undo, now),
        Statement::Delete { table, alias, where_, returning } => exec_delete(db, table, alias.as_deref(), where_.as_ref(), returning.as_deref(), params, undo, now),
        Statement::CreateTable { name, if_not_exists, columns, constraints } => exec_create(db, name, *if_not_exists, columns, constraints, undo),
        Statement::DropTable { names, if_exists } => {
            for n in names {
                let key = n.to_ascii_lowercase();
                match db.tables.remove(&key) {
                    Some(t) => undo.push(Undo::DropTable { table: t }),
                    None if *if_exists => {}
                    None => return Err(SqlErr::new("42P01", format!("table \"{}\" does not exist", n))),
                }
            }
            Ok(QueryResult::update("DROP TABLE", 0))
        }
        Statement::Truncate { name } => {
            let t = db.table_mut(name)?;
            let rows = std::mem::take(&mut t.rows);
            let n = rows.len() as i64;
            undo.push(Undo::Truncate { table: name.to_ascii_lowercase(), rows });
            Ok(QueryResult::update("TRUNCATE TABLE", n))
        }
        Statement::NoOp { command } => Ok(QueryResult::update(command, 0)),
        Statement::Begin | Statement::Commit | Statement::Rollback => Err(SqlErr::new("XX000", "transaction control must be handled by the connection")),
    }
}

fn exec_create(db: &mut Database, name: &str, if_not_exists: bool, defs: &[ColumnDef], constraints: &[TableConstraint], undo: &mut Vec<Undo>) -> SqlResult<QueryResult> {
    let key = name.to_ascii_lowercase();
    if db.tables.contains_key(&key) {
        if if_not_exists {
            return Ok(QueryResult::update("CREATE TABLE", 0));
        }
        return Err(SqlErr::new("42P07", format!("relation \"{}\" already exists", name)));
    }
    let mut columns: Vec<Column> = Vec::new();
    let mut uniques = Vec::new();
    for (i, d) in defs.iter().enumerate() {
        if columns.iter().any(|c| c.name.eq_ignore_ascii_case(&d.name)) {
            return Err(SqlErr::new("42701", format!("column \"{}\" specified more than once", d.name)));
        }
        columns.push(Column { name: d.name.clone(), ty: d.ty.clone(), not_null: d.not_null, default: d.default.clone(), auto_increment: d.auto_increment });
        if d.primary_key {
            uniques.push((vec![i], format!("{}_pkey", name)));
        } else if d.unique {
            uniques.push((vec![i], format!("{}_{}_key", name, d.name)));
        }
    }
    let mut table = Table { name: name.to_string(), columns, rows: Vec::new(), uniques, next_rowid: 1, serial: 1 };
    for c in constraints {
        let (cols, pk) = match c {
            TableConstraint::PrimaryKey(c) => (c, true),
            TableConstraint::Unique(c) => (c, false),
        };
        let mut idx = Vec::new();
        for n in cols {
            let i = table.col_index(n)?;
            if pk {
                table.columns[i].not_null = true;
            }
            idx.push(i);
        }
        if pk && table.uniques.iter().any(|(_, n)| n.ends_with("_pkey")) {
            return Err(SqlErr::new("42P16", format!("multiple primary keys for table \"{}\" are not allowed", name)));
        }
        let cname = if pk { format!("{}_pkey", name) } else { format!("{}_{}_key", name, cols.join("_")) };
        table.uniques.push((idx, cname));
    }
    db.tables.insert(key.clone(), table);
    undo.push(Undo::CreateTable { table: key });
    Ok(QueryResult::update("CREATE TABLE", 0))
}

fn empty_env<'a>(scope: &'a Scope, params: &'a Params<'a>, db: &'a Database, now: &'a str) -> Env<'a> {
    Env { scope, row: &[], group: None, params, db, outer: None, now }
}

fn is_default_marker(e: &Expr) -> bool {
    matches!(e, Expr::Function { name, args, .. } if name == "DEFAULT" && args.is_empty())
}

fn returning_result(table: &Table, items: &[SelectItem], rows: &[Vec<Value>], params: &Params, db: &Database, now: &str, command: &str) -> SqlResult<QueryResult> {
    let scope = table.scope(None);
    let (names, types, exprs) = expand_items(items, &scope)?;
    let mut out = Vec::new();
    for r in rows {
        let env = Env { scope: &scope, row: r, group: None, params, db, outer: None, now };
        let mut o = Vec::new();
        for e in &exprs {
            o.push(eval(e, &env)?);
        }
        out.push(o);
    }
    let types = finish_types(types, &out);
    Ok(QueryResult { columns: names, column_types: types, update_count: rows.len() as i64, rows: out, is_query: true, command: command.into(), generated_keys: Vec::new() })
}

#[allow(clippy::too_many_arguments)]
fn exec_insert(
    db: &mut Database,
    table_name: &str,
    columns: Option<&[String]>,
    source: &InsertSource,
    returning: Option<&[SelectItem]>,
    params: &Params,
    undo: &mut Vec<Undo>,
    now: &str,
) -> SqlResult<QueryResult> {
    // phase 1: compute the rows (immutable)
    let rows: Vec<Vec<Option<Value>>> = {
        let t = db.table(table_name)?;
        let targets: Vec<usize> = match columns {
            Some(cols) => {
                let mut v = Vec::new();
                for c in cols {
                    let i = t.col_index(c)?;
                    if v.contains(&i) {
                        return Err(SqlErr::new("42701", format!("column \"{}\" specified more than once", c)));
                    }
                    v.push(i);
                }
                v
            }
            None => (0..t.columns.len()).collect(),
        };
        let empty = Scope::default();
        let env = empty_env(&empty, params, db, now);
        let provided: Vec<Vec<Option<Value>>> = match source {
            InsertSource::DefaultValues => vec![Vec::new()],
            InsertSource::Values(rows) => {
                let mut out = Vec::new();
                for r in rows {
                    let mut o = Vec::new();
                    for e in r {
                        o.push(if is_default_marker(e) { None } else { Some(eval(e, &env)?) });
                    }
                    out.push(o);
                }
                out
            }
            InsertSource::Select(q) => run_select(db, q, params, None, now)?.rows.into_iter().map(|r| r.into_iter().map(Some).collect()).collect(),
        };
        let mut full = Vec::new();
        for p in provided {
            if p.len() > targets.len() {
                return Err(SqlErr::new("42601", "INSERT has more expressions than target columns"));
            }
            if p.len() < targets.len() && !matches!(source, InsertSource::DefaultValues) && columns.is_some() {
                return Err(SqlErr::new("42601", "INSERT has more target columns than expressions"));
            }
            let mut row: Vec<Option<Value>> = vec![None; t.columns.len()];
            let mut explicit = vec![false; t.columns.len()];
            for (k, v) in p.into_iter().enumerate() {
                if let Some(v) = v {
                    row[targets[k]] = Some(v);
                    explicit[targets[k]] = true;
                }
            }
            for (i, c) in t.columns.iter().enumerate() {
                if explicit[i] {
                    if c.auto_increment && row[i].as_ref().is_some_and(|v| v.is_null()) {
                        row[i] = None;
                    }
                    continue;
                }
                if c.auto_increment {
                    row[i] = None;
                } else if let Some(d) = &c.default {
                    row[i] = Some(eval(d, &env)?);
                } else {
                    row[i] = Some(Value::Null);
                }
            }
            full.push(row);
        }
        full
    };
    // phase 2: apply (mutable)
    let key = table_name.to_ascii_lowercase();
    let t = db.table_mut(table_name)?;
    let auto_cols: Vec<usize> = t.columns.iter().enumerate().filter(|(_, c)| c.auto_increment).map(|(i, _)| i).collect();
    let mut generated = Vec::new();
    let mut inserted = Vec::new();
    for row in rows {
        let mut values = Vec::with_capacity(row.len());
        for (i, v) in row.into_iter().enumerate() {
            let c = &t.columns[i];
            let v = match v {
                Some(v) => {
                    let v = coerce(v, &c.ty).map_err(|e| e.in_column(&c.name))?;
                    if c.auto_increment {
                        if let Value::Int(n) = v {
                            if n >= t.serial {
                                t.serial = n + 1;
                            }
                        }
                    }
                    v
                }
                None => {
                    let n = t.serial;
                    t.serial += 1;
                    Value::Int(n)
                }
            };
            if v.is_null() && c.not_null {
                return Err(SqlErr::new("23502", format!("null value in column \"{}\" of relation \"{}\" violates not-null constraint", c.name, t.name)));
            }
            values.push(v);
        }
        if !auto_cols.is_empty() {
            generated.push(auto_cols.iter().map(|i| (t.columns[*i].name.clone(), values[*i].clone())).collect::<BTreeMap<_, _>>());
        }
        let id = t.next_rowid;
        t.next_rowid += 1;
        inserted.push(values.clone());
        t.rows.push(Row { id, values });
        undo.push(Undo::Insert { table: key.clone(), rowid: id });
    }
    t.validate()?;
    let n = inserted.len() as i64;
    let mut res = match returning {
        Some(items) => {
            let t = db.table(table_name)?;
            returning_result(t, items, &inserted, params, db, now, "INSERT")?
        }
        None => QueryResult::update("INSERT", n),
    };
    res.generated_keys = generated;
    Ok(res)
}

#[allow(clippy::too_many_arguments)]
fn exec_update(
    db: &mut Database,
    table_name: &str,
    alias: Option<&str>,
    sets: &[(String, Expr)],
    where_: Option<&Expr>,
    returning: Option<&[SelectItem]>,
    params: &Params,
    undo: &mut Vec<Undo>,
    now: &str,
) -> SqlResult<QueryResult> {
    let changes: Vec<(usize, Vec<Value>)> = {
        let t = db.table(table_name)?;
        let scope = t.scope(alias);
        let mut targets = Vec::new();
        for (c, e) in sets {
            targets.push((t.col_index(c)?, e));
        }
        let mut out = Vec::new();
        for (ri, r) in t.rows.iter().enumerate() {
            let env = Env { scope: &scope, row: &r.values, group: None, params, db, outer: None, now };
            if let Some(w) = where_ {
                if truth(&eval(w, &env)?)? != Some(true) {
                    continue;
                }
            }
            let mut nv = r.values.clone();
            for (ci, e) in &targets {
                let c = &t.columns[*ci];
                let v = if is_default_marker(e) {
                    match &c.default {
                        Some(d) => eval(d, &env)?,
                        None => Value::Null,
                    }
                } else {
                    eval(e, &env)?
                };
                nv[*ci] = coerce(v, &c.ty).map_err(|e| e.in_column(&c.name))?;
                if nv[*ci].is_null() && c.not_null {
                    return Err(SqlErr::new("23502", format!("null value in column \"{}\" of relation \"{}\" violates not-null constraint", c.name, t.name)));
                }
            }
            out.push((ri, nv));
        }
        out
    };
    let key = table_name.to_ascii_lowercase();
    let t = db.table_mut(table_name)?;
    let mut updated = Vec::new();
    for (ri, nv) in changes {
        let row = &mut t.rows[ri];
        let old = std::mem::replace(&mut row.values, nv.clone());
        undo.push(Undo::Update { table: key.clone(), rowid: row.id, old });
        updated.push(nv);
    }
    t.validate()?;
    let n = updated.len() as i64;
    match returning {
        Some(items) => {
            let t = db.table(table_name)?;
            returning_result(t, items, &updated, params, db, now, "UPDATE")
        }
        None => Ok(QueryResult::update("UPDATE", n)),
    }
}

#[allow(clippy::too_many_arguments)]
fn exec_delete(
    db: &mut Database,
    table_name: &str,
    alias: Option<&str>,
    where_: Option<&Expr>,
    returning: Option<&[SelectItem]>,
    params: &Params,
    undo: &mut Vec<Undo>,
    now: &str,
) -> SqlResult<QueryResult> {
    let doomed: Vec<usize> = {
        let t = db.table(table_name)?;
        let scope = t.scope(alias);
        let mut out = Vec::new();
        for (ri, r) in t.rows.iter().enumerate() {
            if let Some(w) = where_ {
                let env = Env { scope: &scope, row: &r.values, group: None, params, db, outer: None, now };
                if truth(&eval(w, &env)?)? != Some(true) {
                    continue;
                }
            }
            out.push(ri);
        }
        out
    };
    let key = table_name.to_ascii_lowercase();
    let t = db.table_mut(table_name)?;
    let mut removed = Vec::new();
    for ri in doomed.into_iter().rev() {
        let row = t.rows.remove(ri);
        removed.push(row.values.clone());
        undo.push(Undo::Delete { table: key.clone(), row });
    }
    removed.reverse();
    let n = removed.len() as i64;
    match returning {
        Some(items) => {
            let t = db.table(table_name)?;
            returning_result(t, items, &removed, params, db, now, "DELETE")
        }
        None => Ok(QueryResult::update("DELETE", n)),
    }
}

// ---------------------------------------------------------------------------
// SELECT
// ---------------------------------------------------------------------------

fn table_source(db: &Database, tref: &TableRef, params: &Params, outer: Option<&Env>, now: &str) -> SqlResult<(Vec<ScopeCol>, Vec<Vec<Value>>)> {
    match tref {
        TableRef::Table { name, alias } => {
            let t = db.table(name)?;
            let scope = t.scope(alias.as_deref());
            Ok((scope.cols, t.rows.iter().map(|r| r.values.clone()).collect()))
        }
        TableRef::Subquery { query, alias } => {
            let rel = run_select(db, query, params, outer, now)?;
            let cols = rel.columns.iter().map(|c| ScopeCol { table: Some(alias.clone()), name: c.clone(), ty: None }).collect();
            Ok((cols, rel.rows))
        }
    }
}

fn build_source(db: &Database, from: Option<&From>, params: &Params, outer: Option<&Env>, now: &str) -> SqlResult<(Scope, Vec<Vec<Value>>)> {
    let Some(from) = from else {
        return Ok((Scope::default(), vec![Vec::new()]));
    };
    let (cols, mut rows) = table_source(db, &from.base, params, outer, now)?;
    let mut scope = Scope { cols };
    for j in &from.joins {
        let (rcols, rrows) = table_source(db, &j.table, params, outer, now)?;
        let lw = scope.cols.len();
        let rw = rcols.len();
        let mut combined_scope = scope.clone();
        combined_scope.cols.extend(rcols);
        let matches = |l: &[Value], r: &[Value]| -> SqlResult<(bool, Vec<Value>)> {
            let mut row = Vec::with_capacity(lw + rw);
            row.extend_from_slice(l);
            row.extend_from_slice(r);
            let ok = match &j.on {
                None => true,
                Some(on) => {
                    let env = Env { scope: &combined_scope, row: &row, group: None, params, db, outer, now };
                    truth(&eval(on, &env)?)? == Some(true)
                }
            };
            Ok((ok, row))
        };
        let mut out = Vec::new();
        match j.kind {
            JoinKind::Right => {
                for r in &rrows {
                    let mut hit = false;
                    for l in &rows {
                        let (ok, row) = matches(l, r)?;
                        if ok {
                            hit = true;
                            out.push(row);
                        }
                    }
                    if !hit {
                        let mut row = vec![Value::Null; lw];
                        row.extend_from_slice(r);
                        out.push(row);
                    }
                }
            }
            _ => {
                for l in &rows {
                    let mut hit = false;
                    for r in &rrows {
                        let (ok, row) = matches(l, r)?;
                        if ok {
                            hit = true;
                            out.push(row);
                        }
                    }
                    if !hit && j.kind == JoinKind::Left {
                        let mut row = l.clone();
                        row.extend(std::iter::repeat_n(Value::Null, rw));
                        out.push(row);
                    }
                }
            }
        }
        rows = out;
        scope = combined_scope;
    }
    Ok((scope, rows))
}

/// Output name of an expression.
fn expr_name(e: &Expr, scope: &Scope) -> String {
    match e {
        Expr::Column { table, name } => match scope.resolve(table.as_deref(), name) {
            Ok(Some(i)) => scope.cols[i].name.clone(),
            _ => name.clone(),
        },
        Expr::Function { name, .. } => match name.as_str() {
            "__NOT_DISTINCT" => "?column?".into(),
            n => n.to_ascii_lowercase(),
        },
        Expr::Cast { expr, .. } => expr_name(expr, scope),
        Expr::Case { .. } => "case".into(),
        Expr::Exists(_) => "exists".into(),
        _ => "?column?".into(),
    }
}

fn expr_type(e: &Expr, scope: &Scope) -> Option<ColumnType> {
    match e {
        Expr::Column { table, name } => match scope.resolve(table.as_deref(), name) {
            Ok(Some(i)) => scope.cols[i].ty.as_ref().map(column_type),
            _ => None,
        },
        Expr::Cast { ty, .. } => Some(column_type(ty)),
        Expr::Function { name, .. } if name == "COUNT" => Some(ColumnType::BigInt),
        Expr::Function { name, .. } if name == "NOW" => Some(ColumnType::Timestamp),
        _ => None,
    }
}

type Expanded = (Vec<String>, Vec<Option<ColumnType>>, Vec<Expr>);

fn expand_items(items: &[SelectItem], scope: &Scope) -> SqlResult<Expanded> {
    let (mut names, mut types, mut exprs) = (Vec::new(), Vec::new(), Vec::new());
    for it in items {
        match it {
            SelectItem::Wildcard => {
                if scope.cols.is_empty() {
                    return Err(SqlErr::new("42601", "SELECT * with no tables specified is not valid"));
                }
                for c in &scope.cols {
                    names.push(c.name.clone());
                    types.push(c.ty.as_ref().map(column_type));
                    exprs.push(Expr::Column { table: c.table.clone(), name: c.name.clone() });
                }
            }
            SelectItem::QualifiedWildcard(t) => {
                if !scope.has_table(t) {
                    return Err(SqlErr::new("42P01", format!("missing FROM-clause entry for table \"{}\"", t)));
                }
                for c in scope.cols.iter().filter(|c| c.table.as_deref().is_some_and(|ct| ct.eq_ignore_ascii_case(t))) {
                    names.push(c.name.clone());
                    types.push(c.ty.as_ref().map(column_type));
                    exprs.push(Expr::Column { table: c.table.clone(), name: c.name.clone() });
                }
            }
            SelectItem::Expr { expr, alias } => {
                names.push(alias.clone().unwrap_or_else(|| expr_name(expr, scope)));
                types.push(expr_type(expr, scope));
                exprs.push(expr.clone());
            }
        }
    }
    Ok((names, types, exprs))
}

fn finish_types(types: Vec<Option<ColumnType>>, rows: &[Vec<Value>]) -> Vec<ColumnType> {
    types
        .into_iter()
        .enumerate()
        .map(|(i, t)| t.unwrap_or_else(|| rows.iter().map(|r| &r[i]).find(|v| !v.is_null()).map(ColumnType::of_value).unwrap_or(ColumnType::Text)))
        .collect()
}

/// Rows with their ORDER BY keys.
struct Core {
    names: Vec<String>,
    types: Vec<Option<ColumnType>>,
    rows: Vec<(Vec<Value>, Vec<Value>)>,
}

fn order_key(item: &OrderItem, names: &[String], out: &[Value], env: Option<&Env>) -> SqlResult<Value> {
    match &item.expr {
        Expr::Literal(Value::Int(k)) => {
            let k = *k;
            if k < 1 || k as usize > out.len() {
                return Err(SqlErr::new("42P10", format!("ORDER BY position {} is not in select list", k)));
            }
            return Ok(out[k as usize - 1].clone());
        }
        Expr::Column { table: None, name } => {
            let hits: Vec<usize> = names.iter().enumerate().filter(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(i, _)| i).collect();
            if hits.len() == 1 || (hits.len() > 1 && env.is_none()) {
                return Ok(out[hits[0]].clone());
            }
        }
        _ => {}
    }
    match env {
        Some(env) => eval(&item.expr, env),
        None => Err(SqlErr::new("0A000", "ORDER BY on a UNION result must use output column names or positions")),
    }
}

fn run_core(db: &Database, sel: &Select, params: &Params, outer: Option<&Env>, now: &str, order: &[OrderItem]) -> SqlResult<Core> {
    let (scope, rows) = build_source(db, sel.from.as_ref(), params, outer, now)?;
    let mut filtered = Vec::new();
    for r in rows {
        if let Some(w) = &sel.where_ {
            let env = Env { scope: &scope, row: &r, group: None, params, db, outer, now };
            if truth(&eval(w, &env)?)? != Some(true) {
                continue;
            }
        }
        filtered.push(r);
    }
    let (names, types, exprs) = expand_items(&sel.items, &scope)?;
    let aggregate = !sel.group_by.is_empty()
        || sel.having.is_some()
        || exprs.iter().any(|e| e.contains_aggregate())
        || order.iter().any(|o| o.expr.contains_aggregate());
    let mut out = Vec::new();
    if aggregate {
        let mut groups: Vec<Vec<Vec<Value>>> = Vec::new();
        if sel.group_by.is_empty() {
            groups.push(filtered);
        } else {
            let mut index: HashMap<String, usize> = HashMap::new();
            for r in filtered {
                let env = Env { scope: &scope, row: &r, group: None, params, db, outer, now };
                let mut key = Vec::new();
                for g in &sel.group_by {
                    key.push(match g {
                        Expr::Literal(Value::Int(k)) if *k >= 1 && (*k as usize) <= exprs.len() => eval(&exprs[*k as usize - 1], &env)?,
                        g => eval(g, &env)?,
                    });
                }
                let k = group_key(&key);
                match index.get(&k) {
                    Some(i) => groups[*i].push(r),
                    None => {
                        index.insert(k, groups.len());
                        groups.push(vec![r]);
                    }
                }
            }
        }
        let null_row = vec![Value::Null; scope.cols.len()];
        for g in &groups {
            let rep = g.first().map(|r| r.as_slice()).unwrap_or(&null_row);
            let env = Env { scope: &scope, row: rep, group: Some(g), params, db, outer, now };
            if let Some(h) = &sel.having {
                if truth(&eval(h, &env)?)? != Some(true) {
                    continue;
                }
            }
            let mut o = Vec::with_capacity(exprs.len());
            for e in &exprs {
                o.push(eval(e, &env)?);
            }
            let mut keys = Vec::new();
            for it in order {
                keys.push(order_key(it, &names, &o, Some(&env))?);
            }
            out.push((o, keys));
        }
    } else {
        for r in &filtered {
            let env = Env { scope: &scope, row: r, group: None, params, db, outer, now };
            let mut o = Vec::with_capacity(exprs.len());
            for e in &exprs {
                o.push(eval(e, &env)?);
            }
            let mut keys = Vec::new();
            for it in order {
                keys.push(order_key(it, &names, &o, Some(&env))?);
            }
            out.push((o, keys));
        }
    }
    if sel.distinct {
        let mut seen = HashSet::new();
        out.retain(|(o, _)| seen.insert(group_key(o)));
    }
    Ok(Core { names, types, rows: out })
}

fn sort_rows(rows: &mut [(Vec<Value>, Vec<Value>)], order: &[OrderItem]) {
    if order.is_empty() {
        return;
    }
    rows.sort_by(|(_, a), (_, b)| {
        for (i, it) in order.iter().enumerate() {
            let (x, y) = (&a[i], &b[i]);
            let nulls_first = it.nulls_first.unwrap_or(it.desc);
            let o = match (x.is_null(), y.is_null()) {
                (true, true) => Ordering::Equal,
                (true, false) => {
                    if nulls_first {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                }
                (false, true) => {
                    if nulls_first {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
                _ => {
                    let o = compare(x, y).ok().flatten().unwrap_or(Ordering::Equal);
                    if it.desc {
                        o.reverse()
                    } else {
                        o
                    }
                }
            };
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    });
}

/// Run a SELECT query.
pub fn run_select(db: &Database, sel: &Select, params: &Params, outer: Option<&Env>, now: &str) -> SqlResult<Relation> {
    let core_order: &[OrderItem] = if sel.unions.is_empty() { &sel.order_by } else { &[] };
    let mut core = run_core(db, sel, params, outer, now, core_order)?;
    if !sel.unions.is_empty() {
        for (all, part) in &sel.unions {
            let p = run_core(db, part, params, outer, now, &[])?;
            if p.names.len() != core.names.len() {
                return Err(SqlErr::new("42601", "each UNION query must have the same number of columns"));
            }
            core.rows.extend(p.rows);
            if !*all {
                let mut seen = HashSet::new();
                core.rows.retain(|(o, _)| seen.insert(group_key(o)));
            }
        }
        for (o, keys) in core.rows.iter_mut() {
            *keys = sel.order_by.iter().map(|it| order_key(it, &core.names, o, None)).collect::<SqlResult<Vec<_>>>()?;
        }
    }
    sort_rows(&mut core.rows, &sel.order_by);
    let empty = Scope::default();
    let env = empty_env(&empty, params, db, now);
    let offset = match &sel.offset {
        Some(e) => int_of(&eval(e, &env)?, "OFFSET")?.max(0) as usize,
        None => 0,
    };
    let limit = match &sel.limit {
        Some(e) => {
            let v = eval(e, &env)?;
            if v.is_null() {
                None
            } else {
                let n = int_of(&v, "LIMIT")?;
                if n < 0 {
                    return Err(SqlErr::new("2201W", "LIMIT must not be negative"));
                }
                Some(n as usize)
            }
        }
        None => None,
    };
    let rows: Vec<Vec<Value>> = core.rows.into_iter().map(|(o, _)| o).skip(offset).take(limit.unwrap_or(usize::MAX)).collect();
    let types = finish_types(core.types, &rows);
    Ok(Relation { columns: core.names, types, rows })
}
