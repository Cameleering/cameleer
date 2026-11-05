//! Storage and statement execution.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use cameleer_core::value::{ObjectMap, Value};

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
    /// Stored procedures and functions by lower-case name.
    pub routines: BTreeMap<String, Routine>,
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
    Routine { name: String, old: Option<Routine> },
    /// ALTER TABLE: the table before (under `key`), now stored under `new_key`.
    Replace { key: String, new_key: String, table: Table },
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
            Undo::Replace { key, new_key, table } => {
                db.tables.remove(&new_key);
                db.tables.insert(key, table);
            }
            Undo::Routine { name, old } => match old {
                Some(r) => {
                    db.routines.insert(name, r);
                }
                None => {
                    db.routines.remove(&name);
                }
            },
        }
    }
}

/// A relation (intermediate or final query result).
pub struct Relation {
    pub columns: Vec<String>,
    pub types: Vec<ColumnType>,
    pub rows: Vec<Vec<Value>>,
}

pub(crate) fn column_type(ty: &DataType) -> ColumnType {
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
    let prepared;
    let stmt = if !db.routines.is_empty() && matches!(stmt, Statement::Select(_) | Statement::Insert { .. } | Statement::Update { .. } | Statement::Delete { .. } | Statement::Call { .. }) {
        let mut s = stmt.clone();
        prepare_routine_calls(db, undo, &mut s, params, now)?;
        prepared = s;
        &prepared
    } else {
        stmt
    };
    match stmt {
        Statement::CreateRoutine { routine, or_replace } => {
            let old = db.routines.get(&routine.name).cloned();
            if old.is_some() && !*or_replace {
                return Err(SqlErr::new("42723", format!("{} \"{}\" already exists", if routine.function { "function" } else { "procedure" }, routine.name)));
            }
            db.routines.insert(routine.name.clone(), (**routine).clone());
            undo.push(Undo::Routine { name: routine.name.clone(), old });
            Ok(QueryResult::update(if routine.function { "CREATE FUNCTION" } else { "CREATE PROCEDURE" }, 0))
        }
        Statement::DropRoutine { name, function, if_exists } => {
            let kind = if *function { "function" } else { "procedure" };
            match db.routines.get(name) {
                Some(r) if r.function != *function => Err(SqlErr::new("42809", format!("\"{}\" is not a {}", name, kind))),
                Some(_) => {
                    let old = db.routines.remove(name);
                    undo.push(Undo::Routine { name: name.clone(), old });
                    Ok(QueryResult::update(if *function { "DROP FUNCTION" } else { "DROP PROCEDURE" }, 0))
                }
                None if *if_exists => Ok(QueryResult::update(if *function { "DROP FUNCTION" } else { "DROP PROCEDURE" }, 0)),
                None => Err(SqlErr::new("42883", format!("{} {} does not exist", kind, name))),
            }
        }
        Statement::Call { name, args } => {
            let r = db.routines.get(name).cloned().ok_or_else(|| SqlErr::new("42883", format!("procedure {}() does not exist", name)))?;
            if r.function {
                return Err(SqlErr::new("42809", format!("{}() is not a procedure (to call a function, use SELECT)", name)));
            }
            let vals = {
                let scope = Scope::default();
                let env = empty_env(&scope, params, db, now);
                args.iter().map(|a| eval(a, &env)).collect::<SqlResult<Vec<_>>>()?
            };
            let out = super::plsql::call_routine(&mut super::plsql::DbCtx::Mut(db, undo), &r, vals, true, now)?;
            Ok(out.into_call_result())
        }
        Statement::Do(block) => super::plsql::run_do(&mut super::plsql::DbCtx::Mut(db, undo), block, now),
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
        Statement::AlterTable { name, if_exists, action } => exec_alter(db, name, *if_exists, action, params, undo, now),
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

fn exec_alter(db: &mut Database, name: &str, if_exists: bool, action: &AlterAction, params: &Params, undo: &mut Vec<Undo>, now: &str) -> SqlResult<QueryResult> {
    let key = name.to_ascii_lowercase();
    let Some(old) = db.tables.get(&key).cloned() else {
        if if_exists {
            return Ok(QueryResult::update("ALTER TABLE", 0));
        }
        return Err(SqlErr::new("42P01", format!("relation \"{}\" does not exist", name)));
    };
    let mut t = old.clone();
    let mut new_key = key.clone();
    match action {
        AlterAction::AddColumn { def, if_not_exists } => {
            if t.columns.iter().any(|c| c.name.eq_ignore_ascii_case(&def.name)) {
                if *if_not_exists {
                    return Ok(QueryResult::update("ALTER TABLE", 0));
                }
                return Err(SqlErr::new("42701", format!("column \"{}\" of relation \"{}\" already exists", def.name, t.name)));
            }
            let scope = Scope::default();
            let fill = match &def.default {
                Some(d) => coerce(eval(d, &empty_env(&scope, params, db, now))?, &def.ty)?,
                None => Value::Null,
            };
            let idx = t.columns.len();
            for r in t.rows.iter_mut() {
                let v = if def.auto_increment {
                    let v = Value::Int(t.serial);
                    t.serial += 1;
                    v
                } else {
                    fill.clone()
                };
                r.values.push(v);
            }
            if (def.not_null || def.primary_key) && t.rows.iter().any(|r| r.values[idx].is_null()) {
                return Err(SqlErr::new("23502", format!("column \"{}\" of relation \"{}\" contains null values", def.name, t.name)));
            }
            t.columns.push(Column { name: def.name.clone(), ty: def.ty.clone(), not_null: def.not_null || def.primary_key, default: def.default.clone(), auto_increment: def.auto_increment });
            if def.primary_key {
                if t.uniques.iter().any(|(_, n)| n.ends_with("_pkey")) {
                    return Err(SqlErr::new("42P16", format!("multiple primary keys for table \"{}\" are not allowed", t.name)));
                }
                t.uniques.push((vec![idx], format!("{}_pkey", t.name)));
            } else if def.unique {
                t.uniques.push((vec![idx], format!("{}_{}_key", t.name, def.name)));
            }
        }
        AlterAction::DropColumn { name: col, if_exists } => {
            let Some(i) = t.columns.iter().position(|c| c.name.eq_ignore_ascii_case(col)) else {
                if *if_exists {
                    return Ok(QueryResult::update("ALTER TABLE", 0));
                }
                return Err(SqlErr::new("42703", format!("column \"{}\" of relation \"{}\" does not exist", col, t.name)));
            };
            t.columns.remove(i);
            for r in t.rows.iter_mut() {
                r.values.remove(i);
            }
            t.uniques.retain(|(cols, _)| !cols.contains(&i));
            for (cols, _) in t.uniques.iter_mut() {
                for c in cols.iter_mut() {
                    if *c > i {
                        *c -= 1;
                    }
                }
            }
        }
        AlterAction::RenameColumn { from, to } => {
            let i = t.col_index(from)?;
            if t.columns.iter().any(|c| c.name.eq_ignore_ascii_case(to)) {
                return Err(SqlErr::new("42701", format!("column \"{}\" of relation \"{}\" already exists", to, t.name)));
            }
            t.columns[i].name = to.clone();
        }
        AlterAction::RenameTable { to } => {
            new_key = to.to_ascii_lowercase();
            if db.tables.contains_key(&new_key) {
                return Err(SqlErr::new("42P07", format!("relation \"{}\" already exists", to)));
            }
            t.name = to.clone();
        }
        AlterAction::SetDefault { column, default } => {
            let i = t.col_index(column)?;
            t.columns[i].default = default.clone();
        }
        AlterAction::SetNotNull { column, not_null } => {
            let i = t.col_index(column)?;
            if *not_null && t.rows.iter().any(|r| r.values[i].is_null()) {
                return Err(SqlErr::new("23502", format!("column \"{}\" of relation \"{}\" contains null values", t.columns[i].name, t.name)));
            }
            t.columns[i].not_null = *not_null;
        }
        AlterAction::SetType { column, ty } => {
            let i = t.col_index(column)?;
            for r in t.rows.iter_mut() {
                let v = std::mem::take(&mut r.values[i]);
                r.values[i] = coerce(v, ty)?;
            }
            t.columns[i].ty = ty.clone();
        }
        AlterAction::AddConstraint { name: cname, constraint } => {
            let (cols, pk) = match constraint {
                TableConstraint::PrimaryKey(c) => (c, true),
                TableConstraint::Unique(c) => (c, false),
            };
            let mut idx = Vec::new();
            for n in cols {
                let i = t.col_index(n)?;
                if pk {
                    if t.rows.iter().any(|r| r.values[i].is_null()) {
                        return Err(SqlErr::new("23502", format!("column \"{}\" of relation \"{}\" contains null values", t.columns[i].name, t.name)));
                    }
                    t.columns[i].not_null = true;
                }
                idx.push(i);
            }
            if pk && t.uniques.iter().any(|(_, n)| n.ends_with("_pkey")) {
                return Err(SqlErr::new("42P16", format!("multiple primary keys for table \"{}\" are not allowed", t.name)));
            }
            let cn = cname.clone().unwrap_or_else(|| if pk { format!("{}_pkey", t.name) } else { format!("{}_{}_key", t.name, cols.join("_")) });
            t.uniques.push((idx, cn));
            t.validate()?;
        }
        AlterAction::DropConstraint { name: cname, if_exists } => {
            let before = t.uniques.len();
            t.uniques.retain(|(_, n)| !n.eq_ignore_ascii_case(cname));
            if before == t.uniques.len() && !*if_exists {
                return Err(SqlErr::new("42704", format!("constraint \"{}\" of relation \"{}\" does not exist", cname, t.name)));
            }
        }
    }
    db.tables.remove(&key);
    db.tables.insert(new_key.clone(), t);
    undo.push(Undo::Replace { key, new_key, table: old });
    Ok(QueryResult::update("ALTER TABLE", 0))
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
            generated.push(auto_cols.iter().map(|i| (t.columns[*i].name.clone(), values[*i].clone())).collect::<ObjectMap>());
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
        TableRef::Values { name, columns, rows, alias } => {
            let t = alias.clone().unwrap_or_else(|| name.clone());
            Ok((columns.iter().map(|c| ScopeCol { table: Some(t.clone()), name: c.clone(), ty: None }).collect(), rows.clone()))
        }
        TableRef::Function { name, args, alias } => {
            let scope = Scope::default();
            let base = empty_env(&scope, params, db, now);
            let env = match outer {
                Some(o) => Env { outer: Some(o), ..base },
                None => base,
            };
            let vals = args.iter().map(|a| eval(a, &env)).collect::<SqlResult<Vec<_>>>()?;
            let (columns, rows) = table_function(db, name, vals, now)?;
            let t = alias.clone().unwrap_or_else(|| name.clone());
            Ok((columns.iter().map(|c| ScopeCol { table: Some(t.clone()), name: c.clone(), ty: None }).collect(), rows))
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
                let mut right_hit = vec![false; rrows.len()];
                for l in &rows {
                    let mut hit = false;
                    for (ri, r) in rrows.iter().enumerate() {
                        let (ok, row) = matches(l, r)?;
                        if ok {
                            hit = true;
                            right_hit[ri] = true;
                            out.push(row);
                        }
                    }
                    if !hit && matches!(j.kind, JoinKind::Left | JoinKind::Full) {
                        let mut row = l.clone();
                        row.extend(std::iter::repeat_n(Value::Null, rw));
                        out.push(row);
                    }
                }
                if j.kind == JoinKind::Full {
                    for (ri, r) in rrows.iter().enumerate() {
                        if !right_hit[ri] {
                            let mut row = vec![Value::Null; lw];
                            row.extend_from_slice(r);
                            out.push(row);
                        }
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
        let mut kept: Vec<&Vec<Vec<Value>>> = Vec::new();
        for g in &groups {
            let rep = g.first().map(|r| r.as_slice()).unwrap_or(&null_row);
            let env = Env { scope: &scope, row: rep, group: Some(g), params, db, outer, now };
            if let Some(h) = &sel.having {
                if truth(&eval(h, &env)?)? != Some(true) {
                    continue;
                }
            }
            kept.push(g);
        }
        let order_exprs: Vec<Expr> = order.iter().map(|o| o.expr.clone()).collect();
        let windows = collect_windows(&[exprs.clone(), order_exprs].concat());
        fn env_of<'g>(g: &'g [Vec<Value>], null_row: &'g [Value]) -> (&'g [Value], &'g [Vec<Value>]) {
            (g.first().map(|r| r.as_slice()).unwrap_or(null_row), g)
        }
        let win_vals: Vec<Vec<Value>> = if windows.is_empty() {
            Vec::new()
        } else {
            let envs: Vec<Env> = kept
                .iter()
                .map(|g| {
                    let (rep, g) = env_of(g, &null_row);
                    Env { scope: &scope, row: rep, group: Some(g), params, db, outer, now }
                })
                .collect();
            windows.iter().map(|w| compute_window(w, &envs, None)).collect::<SqlResult<_>>()?
        };
        for (gi, g) in kept.iter().enumerate() {
            let (rep, g) = env_of(g, &null_row);
            let env = Env { scope: &scope, row: rep, group: Some(g), params, db, outer, now };
            let mut o = Vec::with_capacity(exprs.len());
            let mut k = 0;
            for e in &exprs {
                o.push(if win_vals.is_empty() { eval(e, &env)? } else { eval(&subst_windows(e, &win_vals, gi, &mut k), &env)? });
            }
            let mut keys = Vec::new();
            for it in order {
                if win_vals.is_empty() {
                    keys.push(order_key(it, &names, &o, Some(&env))?);
                } else {
                    let e2 = subst_windows(&it.expr, &win_vals, gi, &mut k);
                    if matches!(it.expr, Expr::Window { .. }) {
                        // a window value, not an output position
                        keys.push(eval(&e2, &env)?);
                    } else {
                        let it2 = OrderItem { expr: e2, desc: it.desc, nulls_first: it.nulls_first };
                        keys.push(order_key(&it2, &names, &o, Some(&env))?);
                    }
                }
            }
            out.push((o, keys));
        }
    } else {
        let order_exprs: Vec<Expr> = order.iter().map(|o| o.expr.clone()).collect();
        let windows = collect_windows(&[exprs.clone(), order_exprs].concat());
        let win_vals: Vec<Vec<Value>> = if windows.is_empty() {
            Vec::new()
        } else {
            let envs: Vec<Env> = filtered.iter().map(|r| Env { scope: &scope, row: r, group: None, params, db, outer, now }).collect();
            windows.iter().map(|w| compute_window(w, &envs, Some(&filtered))).collect::<SqlResult<_>>()?
        };
        for (ri, r) in filtered.iter().enumerate() {
            let env = Env { scope: &scope, row: r, group: None, params, db, outer, now };
            let mut o = Vec::with_capacity(exprs.len());
            let mut k = 0;
            for e in &exprs {
                if win_vals.is_empty() {
                    o.push(eval(e, &env)?);
                } else {
                    o.push(eval(&subst_windows(e, &win_vals, ri, &mut k), &env)?);
                }
            }
            let mut keys = Vec::new();
            for it in order {
                if win_vals.is_empty() {
                    keys.push(order_key(it, &names, &o, Some(&env))?);
                } else {
                    let e2 = subst_windows(&it.expr, &win_vals, ri, &mut k);
                    if matches!(it.expr, Expr::Window { .. }) {
                        // a window value, not an output position
                        keys.push(eval(&e2, &env)?);
                    } else {
                        let it2 = OrderItem { expr: e2, desc: it.desc, nulls_first: it.nulls_first };
                        keys.push(order_key(&it2, &names, &o, Some(&env))?);
                    }
                }
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
    if !sel.ctes.is_empty() {
        return run_with_ctes(db, sel, params, outer, now);
    }
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

/// Replaces references to a CTE by its materialized rows.
fn subst_cte(sel: &mut Select, name: &str, columns: &[String], rows: &[Vec<Value>]) {
    sel.walk_tables_mut(&mut |t| {
        if let TableRef::Table { name: n, alias } = t {
            if n.eq_ignore_ascii_case(name) {
                *t = TableRef::Values { name: name.to_string(), columns: columns.to_vec(), rows: rows.to_vec(), alias: alias.clone() };
            }
        }
    });
}

fn references_table(sel: &Select, name: &str) -> bool {
    let mut s = sel.clone();
    let mut hit = false;
    s.walk_tables_mut(&mut |t| {
        if let TableRef::Table { name: n, .. } = t {
            if n.eq_ignore_ascii_case(name) {
                hit = true;
            }
        }
    });
    hit
}

fn run_with_ctes(db: &Database, sel: &Select, params: &Params, outer: Option<&Env>, now: &str) -> SqlResult<Relation> {
    let mut main = sel.clone();
    let ctes = std::mem::take(&mut main.ctes);
    let mut done: Vec<(String, Vec<String>, Vec<Vec<Value>>)> = Vec::new();
    for cte in ctes {
        let mut q = (*cte.query).clone();
        for (n, c, r) in &done {
            subst_cte(&mut q, n, c, r);
        }
        let rename = |cols: Vec<String>| -> SqlResult<Vec<String>> {
            match &cte.columns {
                Some(names) if names.len() != cols.len() => Err(SqlErr::new("42P10", format!("WITH query \"{}\" has {} columns available but {} columns specified", cte.name, cols.len(), names.len()))),
                Some(names) => Ok(names.clone()),
                None => Ok(cols),
            }
        };
        let (cols, rows) = if cte.recursive && references_table(&q, &cte.name) {
            let mut anchor = q.clone();
            let parts = std::mem::take(&mut anchor.unions);
            anchor.order_by.clear();
            if parts.is_empty() || references_table(&anchor, &cte.name) {
                return Err(SqlErr::new("42P19", format!("recursive query \"{}\" must have the form non-recursive-term UNION [ALL] recursive-term", cte.name)));
            }
            let first = run_select(db, &anchor, params, outer, now)?;
            let cols = rename(first.columns)?;
            let mut seen: HashSet<String> = HashSet::new();
            let distinct = parts.iter().any(|(all, _)| !*all);
            let mut all_rows = Vec::new();
            for r in first.rows {
                if !distinct || seen.insert(group_key(&r)) {
                    all_rows.push(r);
                }
            }
            let mut working = all_rows.clone();
            let mut iterations = 0;
            while !working.is_empty() {
                iterations += 1;
                if iterations > 100_000 || all_rows.len() > 1_000_000 {
                    return Err(SqlErr::new("54001", format!("recursive query \"{}\" does not terminate", cte.name)));
                }
                let mut fresh = Vec::new();
                for (_, part) in &parts {
                    let mut p = part.clone();
                    subst_cte(&mut p, &cte.name, &cols, &working);
                    for r in run_select(db, &p, params, outer, now)?.rows {
                        if !distinct || seen.insert(group_key(&r)) {
                            fresh.push(r);
                        }
                    }
                }
                all_rows.extend(fresh.iter().cloned());
                working = fresh;
            }
            (cols, all_rows)
        } else {
            let r = run_select(db, &q, params, outer, now)?;
            (rename(r.columns)?, r.rows)
        };
        done.push((cte.name.clone(), cols, rows));
    }
    for (n, c, r) in &done {
        subst_cte(&mut main, n, c, r);
    }
    run_select(db, &main, params, outer, now)
}

/// Window expressions of a list of expressions, in [`Expr::walk_mut`] order.
fn collect_windows(exprs: &[Expr]) -> Vec<Expr> {
    let mut out = Vec::new();
    for e in exprs {
        let mut c = e.clone();
        c.walk_mut(&mut |x| {
            if let Expr::Window { .. } = x {
                out.push(x.clone());
            }
        });
    }
    out
}

/// Replaces window expressions (in walk order, continuing at `*k`) by literal values.
fn subst_windows(e: &Expr, vals: &[Vec<Value>], row: usize, k: &mut usize) -> Expr {
    let mut c = e.clone();
    c.walk_mut(&mut |x| {
        if let Expr::Window { .. } = x {
            *x = Expr::Literal(vals[*k][row].clone());
            *k += 1;
        }
    });
    c
}

/// Values of one window expression for every row (`envs[i]` evaluates row i; `rows` are the
/// source rows for window aggregates).
fn compute_window(win: &Expr, envs: &[Env], rows: Option<&[Vec<Value>]>) -> SqlResult<Vec<Value>> {
    let Expr::Window { func, partition, order } = win else { unreachable!() };
    let n = envs.len();
    let mut out = vec![Value::Null; n];
    // partitions in first-seen order
    let mut parts: Vec<Vec<usize>> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (i, env) in envs.iter().enumerate() {
        let key: Vec<Value> = partition.iter().map(|p| eval(p, env)).collect::<SqlResult<_>>()?;
        let k = group_key(&key);
        match index.get(&k) {
            Some(p) => parts[*p].push(i),
            None => {
                index.insert(k, parts.len());
                parts.push(vec![i]);
            }
        }
    }
    let (name, args) = match &**func {
        Expr::Function { name, args, .. } => (name.as_str(), args.as_slice()),
        _ => return Err(SqlErr::new("42809", "OVER requires a function call")),
    };
    for part in parts {
        // sort by the ORDER BY keys
        let mut keyed: Vec<((Vec<Value>, Vec<Value>), usize)> = Vec::new();
        for &i in &part {
            let keys: Vec<Value> = order.iter().map(|o| eval(&o.expr, &envs[i])).collect::<SqlResult<_>>()?;
            keyed.push(((Vec::new(), keys), i));
        }
        let mut sortable: Vec<(Vec<Value>, Vec<Value>)> = keyed.iter().map(|((_, k), i)| (vec![Value::Int(*i as i64)], k.clone())).collect();
        sort_rows(&mut sortable, order);
        let sorted: Vec<usize> = sortable.iter().map(|(i, _)| i[0].as_i64().unwrap() as usize).collect();
        let keys: Vec<Vec<Value>> = sortable.into_iter().map(|(_, k)| k).collect();
        let m = sorted.len();
        let peer = |a: usize, b: usize| group_key(&keys[a]) == group_key(&keys[b]);
        // last peer index of each position
        let mut last_peer = vec![0; m];
        let mut j = m;
        while j > 0 {
            j -= 1;
            last_peer[j] = if j + 1 < m && peer(j, j + 1) { last_peer[j + 1] } else { j };
        }
        let arg = |k: usize, pos: usize| -> SqlResult<Value> {
            match args.get(k) {
                Some(a) => eval(a, &envs[sorted[pos]]),
                None => Ok(Value::Null),
            }
        };
        let const_int = |k: usize, default: i64| -> SqlResult<i64> {
            match args.get(k) {
                Some(a) => int_of(&eval(a, &envs[sorted[0]])?, name),
                None => Ok(default),
            }
        };
        let mut rank = 0usize;
        let mut dense = 0usize;
        for pos in 0..m {
            if pos == 0 || !peer(pos, pos - 1) {
                rank = pos + 1;
                dense += 1;
            }
            let v = match name {
                "ROW_NUMBER" => Value::Int(pos as i64 + 1),
                "RANK" => Value::Int(rank as i64),
                "DENSE_RANK" => Value::Int(dense as i64),
                "PERCENT_RANK" => Value::Float(if m > 1 { (rank - 1) as f64 / (m - 1) as f64 } else { 0.0 }),
                "CUME_DIST" => Value::Float((last_peer[pos] + 1) as f64 / m as f64),
                "NTILE" => {
                    let buckets = const_int(0, 1)?.max(1) as usize;
                    let (q, r) = (m / buckets, m % buckets);
                    // the first r buckets have q + 1 rows
                    let b = if pos < r * (q + 1) { pos / (q + 1) } else { r + (pos - r * (q + 1)) / q.max(1) };
                    Value::Int(b as i64 + 1)
                }
                "LAG" | "LEAD" => {
                    let off = const_int(1, 1)?;
                    let target = if name == "LAG" { pos as i64 - off } else { pos as i64 + off };
                    if target >= 0 && (target as usize) < m {
                        arg(0, target as usize)?
                    } else {
                        arg(2, pos)?
                    }
                }
                "FIRST_VALUE" => arg(0, 0)?,
                "LAST_VALUE" => arg(0, if order.is_empty() { m - 1 } else { last_peer[pos] })?,
                "NTH_VALUE" => {
                    let k = const_int(1, 1)?;
                    let end = if order.is_empty() { m - 1 } else { last_peer[pos] };
                    if k >= 1 && (k as usize - 1) <= end {
                        arg(0, k as usize - 1)?
                    } else {
                        Value::Null
                    }
                }
                agg if is_aggregate(agg) => {
                    let rows = rows.ok_or_else(|| SqlErr::new("0A000", "aggregate window functions over grouped rows are not supported"))?;
                    let end = if order.is_empty() { m - 1 } else { last_peer[pos] };
                    let frame: Vec<Vec<Value>> = sorted[..=end].iter().map(|i| rows[*i].clone()).collect();
                    let e = &envs[sorted[pos]];
                    let env = Env { scope: e.scope, row: e.row, group: Some(&frame), params: e.params, db: e.db, outer: e.outer, now: e.now };
                    eval(func, &env)?
                }
                other => return Err(SqlErr::new("42883", format!("function {}() is not a window function", other.to_ascii_lowercase()))),
            };
            out[sorted[pos]] = v;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// routines in queries
// ---------------------------------------------------------------------------

/// A set-returning function in FROM evaluated read-only: a user routine or the built-in
/// `generate_series(start, stop [, step])`.
fn table_function(db: &Database, name: &str, args: Vec<Value>, now: &str) -> SqlResult<(Vec<String>, Vec<Vec<Value>>)> {
    if let Some(r) = db.routines.get(name) {
        if !r.function {
            return Err(SqlErr::new("42809", format!("{}() is a procedure (use CALL)", name)));
        }
        let out = super::plsql::call_routine(&mut super::plsql::DbCtx::Ref(db), r, args, false, now)?;
        return Ok((out.columns, out.rows));
    }
    if name == "generate_series" {
        if !(2..=3).contains(&args.len()) {
            return Err(SqlErr::new("42883", "function generate_series requires 2 or 3 arguments"));
        }
        if args.iter().any(|a| a.is_null()) {
            return Ok((vec![name.into()], Vec::new()));
        }
        let a = int_of(&args[0], "generate_series start")?;
        let b = int_of(&args[1], "generate_series stop")?;
        let step = if args.len() == 3 { int_of(&args[2], "generate_series step")? } else { 1 };
        if step == 0 {
            return Err(SqlErr::new("22023", "step size cannot equal zero"));
        }
        let mut rows = Vec::new();
        let mut i = a;
        while (step > 0 && i <= b) || (step < 0 && i >= b) {
            rows.push(vec![Value::Int(i)]);
            if rows.len() > 10_000_000 {
                return Err(SqlErr::new("54000", "generate_series: too many rows"));
            }
            i += step;
        }
        return Ok((vec![name.into()], rows));
    }
    Err(SqlErr::new("42883", format!("function {}() does not exist", name)))
}

/// Pre-evaluate calls of user routines whose arguments are constant (literals and
/// parameters) with write access, replacing them by their results, so that functions
/// called as `SELECT fn(...)` / `SELECT * FROM fn(...)` may modify data. Calls with
/// column arguments are evaluated per row (read-only) during the query.
pub fn prepare_routine_calls(db: &mut Database, undo: &mut Vec<Undo>, stmt: &mut Statement, params: &Params, now: &str) -> SqlResult<()> {
    if db.routines.is_empty() {
        return Ok(());
    }
    // keep the output column name of `SELECT fn(...)`
    if let Statement::Select(s) = stmt {
        for it in &mut s.items {
            if let SelectItem::Expr { expr: Expr::Function { name, .. }, alias: alias @ None } = it {
                if db.routines.contains_key(&name.to_ascii_lowercase()) {
                    *alias = Some(name.to_ascii_lowercase());
                }
            }
        }
    }
    let mut err: Option<SqlErr> = None;
    stmt.walk_mut(&mut |e| {
        if err.is_some() {
            return;
        }
        let Expr::Function { name, args, .. } = e else { return };
        let key = name.to_ascii_lowercase();
        let Some(r) = db.routines.get(&key).filter(|r| r.function).cloned() else { return };
        if !args.iter().all(|a| a.is_constant()) {
            return;
        }
        let vals = {
            let scope = Scope::default();
            let env = empty_env(&scope, params, db, now);
            args.iter().map(|a| eval(a, &env)).collect::<SqlResult<Vec<_>>>()
        };
        let r = vals.and_then(|vals| super::plsql::call_routine(&mut super::plsql::DbCtx::Mut(db, undo), &r, vals, false, now));
        match r {
            Ok(out) => *e = Expr::Literal(out.scalar),
            Err(x) => err = Some(x),
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    stmt.walk_tables_mut(&mut |t| {
        if err.is_some() {
            return;
        }
        let TableRef::Function { name, args, alias } = t else { return };
        let Some(r) = db.routines.get(name.as_str()).filter(|r| r.function).cloned() else { return };
        if !args.iter().all(|a| a.is_constant()) {
            return;
        }
        let vals = {
            let scope = Scope::default();
            let env = empty_env(&scope, params, db, now);
            args.iter().map(|a| eval(a, &env)).collect::<SqlResult<Vec<_>>>()
        };
        match vals.and_then(|vals| super::plsql::call_routine(&mut super::plsql::DbCtx::Mut(db, undo), &r, vals, false, now)) {
            Ok(out) => *t = TableRef::Values { name: name.clone(), columns: out.columns, rows: out.rows, alias: alias.clone() },
            Err(x) => err = Some(x),
        }
    });
    match err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
