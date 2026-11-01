//! **rustsql** — an embedded, in-memory, thread-safe SQL engine with a PostgreSQL-flavoured
//! dialect.
//!
//! Databases are shared per name inside the process (`jdbc:rustsql:mem:<name>`): every
//! [`EmbeddedDataSource`] / [`EmbeddedConnection`] for the same name sees the same tables.
//!
//! Supported SQL:
//!
//! * `CREATE TABLE [IF NOT EXISTS]` with `INTEGER`/`INT`/`SMALLINT`, `BIGINT`, `SERIAL`/`BIGSERIAL`,
//!   `AUTO_INCREMENT`/`IDENTITY`/`GENERATED ... AS IDENTITY`, `VARCHAR(n)`/`CHAR(n)`/`TEXT`,
//!   `BOOLEAN`, `DOUBLE`/`FLOAT`/`REAL`, `DECIMAL(p,s)`/`NUMERIC`, `TIMESTAMP`/`DATE`, `BYTEA`/`BLOB`;
//!   column constraints `PRIMARY KEY`, `NOT NULL`, `UNIQUE`, `DEFAULT expr` (`REFERENCES`/`CHECK`
//!   are parsed and ignored); table constraints `PRIMARY KEY (..)`, `UNIQUE (..)`.
//! * `DROP TABLE [IF EXISTS]`, `TRUNCATE`, `CREATE INDEX` (accepted, no-op), `ALTER TABLE [IF EXISTS] t
//!   ADD [COLUMN] [IF NOT EXISTS] | DROP [COLUMN] [IF EXISTS] | RENAME [COLUMN] a TO b | RENAME TO n |
//!   ALTER [COLUMN] c SET/DROP DEFAULT, SET/DROP NOT NULL, [SET DATA] TYPE t | ADD [CONSTRAINT n]
//!   PRIMARY KEY/UNIQUE (..) | DROP CONSTRAINT n` (undone by ROLLBACK).
//! * `INSERT INTO t [(cols)] VALUES (..), (..) | SELECT ... | DEFAULT VALUES [RETURNING ..]`.
//! * `[WITH [RECURSIVE] name [(cols)] AS (query), ...] SELECT [DISTINCT] ... FROM t [alias]
//!   [INNER|LEFT|RIGHT|FULL [OUTER]|CROSS JOIN ... ON|USING] WHERE ...
//!   GROUP BY ... HAVING ... ORDER BY ... [ASC|DESC] [NULLS FIRST|LAST] LIMIT n OFFSET m`,
//!   `UNION [ALL]`, subqueries (`IN (SELECT ..)`, `EXISTS`, scalar, in FROM), `FOR UPDATE` (no-op),
//!   window functions `f(..) OVER ([PARTITION BY ..] [ORDER BY ..])` (ranking, offset, value and
//!   aggregate functions, default frame).
//! * Expressions: `AND`/`OR`/`NOT`, comparisons, `LIKE`/`ILIKE`, `IN`, `BETWEEN`, `IS [NOT] NULL`,
//!   arithmetic, `||`, `CASE`, `CAST(x AS t)`/`x::t`, functions `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`,
//!   `STRING_AGG`, `UPPER`, `LOWER`, `LENGTH`, `COALESCE`, `NULLIF`, `NOW`/`CURRENT_TIMESTAMP`,
//!   `ABS`, `ROUND`, `MOD`, `CONCAT`, `SUBSTRING`, `TRIM`, `REPLACE`, `POSITION`, `GREATEST`/`LEAST`...
//! * `UPDATE t SET .. WHERE .. [RETURNING]`, `DELETE FROM t WHERE .. [RETURNING]`.
//! * `BEGIN`/`START TRANSACTION`, `COMMIT`, `ROLLBACK` (undo-log rollback; isolation is
//!   "read uncommitted": statements are serialized but transactions interleave).
//! * Parameters: `?` (positional), `$n` (numbered) and `:name` / `:#name` (named).
//! * Stored routines ([`plsql`], [`plparse`]): `CREATE [OR REPLACE] PROCEDURE name([IN|OUT|INOUT]
//!   [name] type [DEFAULT expr], ...) [LANGUAGE sql|plpgsql] AS $$ body $$`, `CREATE FUNCTION
//!   ... RETURNS type | SETOF type | SETOF table | TABLE (col type, ...) | void | record`
//!   (`STRICT`, volatility and other attributes accepted), `DROP PROCEDURE|FUNCTION [IF
//!   EXISTS]`, `CALL name(args)` (OUT/INOUT values returned as one row; OUT positions may be
//!   passed as NULL or omitted), `SELECT fn(args)`, `SELECT * FROM fn(args) [alias]`,
//!   `DO $$ block $$`, built-in `generate_series`. Bodies are plain SQL (the last statement
//!   gives the result) or a PL/pgSQL subset: `DECLARE` (types, `RECORD`, `%TYPE`,
//!   `CONSTANT`, `NOT NULL`, defaults), `:=` / `=` assignment (also `rec.field`),
//!   `IF/ELSIF/ELSE`, `WHILE`, `LOOP`, `FOR i IN [REVERSE] a..b [BY n]`, `FOR rec IN
//!   query`, labels with `EXIT`/`CONTINUE [label] [WHEN]`, `RETURN`, `RETURN NEXT`,
//!   `RETURN QUERY`, `RAISE NOTICE|EXCEPTION 'fmt %', args [USING ERRCODE = ..]`, SQL
//!   statements with variables, `SELECT ... INTO [STRICT]`, `... RETURNING ... INTO`,
//!   `PERFORM`, `FOUND`, `GET DIAGNOSTICS ... = ROW_COUNT`, nested blocks with `EXCEPTION
//!   WHEN cond THEN` handlers (block changes rolled back; `SQLSTATE`/`SQLERRM`). A plain
//!   `SELECT` in a procedure without OUT parameters becomes the result set of `CALL`.
//!   Functions called per row with column arguments run read-only; data-modifying
//!   functions are called with constant arguments (`SELECT fn(1)`, `FROM fn(?)`) or via
//!   procedures. Routine calls nest up to [`plsql::MAX_DEPTH`] levels. Not supported:
//!   overloading, VARIADIC, cursors, `FOREACH`, transaction control inside procedures,
//!   triggers.
//!
//! Errors are `SQLException` kinds with PostgreSQL SQLStates (see [`crate::datasource::sql_error`]).

pub mod ast;
pub mod eval;
pub mod exec;
pub mod lexer;
pub mod parser;
pub mod plast;
pub mod plparse;
pub mod plsql;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use camel_core::error::{CamelError, Result};
use camel_core::value::{ObjectMap, Value};

use crate::datasource::{sql_error, Connection, DataSource, ExecuteOptions, QueryResult};
use ast::Statement;
use eval::Params;
use exec::{apply_undo, execute_statement, Database, Undo};

/// An engine error with a SQLState code.
#[derive(Clone, Debug, PartialEq)]
pub struct SqlErr {
    pub code: &'static str,
    pub message: String,
}

impl SqlErr {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        SqlErr { code, message: message.into() }
    }
    pub fn syntax(message: impl Into<String>) -> Self {
        SqlErr::new("42601", message)
    }
    fn in_column(mut self, col: &str) -> Self {
        self.message = format!("{} (column \"{}\")", self.message, col);
        self
    }
}

impl std::fmt::Display for SqlErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} [SQLState: {}]", self.message, self.code)
    }
}

impl From<SqlErr> for CamelError {
    fn from(e: SqlErr) -> Self {
        sql_error(e.code, e.message)
    }
}

/// Engine result type.
pub type SqlResult<T> = std::result::Result<T, SqlErr>;

/// A named database shared by all connections to it.
pub struct SharedDatabase {
    name: String,
    db: Mutex<Database>,
}

impl SharedDatabase {
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Names of the existing tables.
    pub fn table_names(&self) -> Vec<String> {
        self.db.lock().unwrap_or_else(|e| e.into_inner()).table_names()
    }
}

static DATABASES: Mutex<BTreeMap<String, Arc<SharedDatabase>>> = Mutex::new(BTreeMap::new());

/// Get (or create) the shared in-memory database with this name.
pub fn database(name: &str) -> Arc<SharedDatabase> {
    let mut m = DATABASES.lock().unwrap_or_else(|e| e.into_inner());
    m.entry(name.to_string()).or_insert_with(|| Arc::new(SharedDatabase { name: name.to_string(), db: Mutex::new(Database::default()) })).clone()
}

/// Forget a named database (existing connections keep their handle). Returns true if it existed.
pub fn drop_database(name: &str) -> bool {
    DATABASES.lock().unwrap_or_else(|e| e.into_inner()).remove(name).is_some()
}

fn now_text() -> String {
    camel_core::util_time::format_millis(camel_core::util_time::now_millis(), "yyyy-MM-dd HH:mm:ss.SSS")
}

/// A connection to an embedded database.
pub struct EmbeddedConnection {
    db: Arc<SharedDatabase>,
    tx: Option<Vec<Undo>>,
    aborted: bool,
}

impl EmbeddedConnection {
    /// Open a connection to the named database.
    pub fn open(name: &str) -> Self {
        EmbeddedConnection { db: database(name), tx: None, aborted: false }
    }

    /// PostgreSQL transaction status byte: `I` idle, `T` in transaction, `E` failed transaction.
    pub fn transaction_status(&self) -> u8 {
        match (&self.tx, self.aborted) {
            (None, _) => b'I',
            (Some(_), false) => b'T',
            (Some(_), true) => b'E',
        }
    }

    /// Mark an open transaction as failed (as if a statement had raised an error).
    pub fn mark_failed(&mut self) {
        if self.tx.is_some() {
            self.aborted = true;
        }
    }

    fn do_rollback(&mut self) {
        if let Some(undo) = self.tx.take() {
            let mut db = self.db.db.lock().unwrap_or_else(|e| e.into_inner());
            apply_undo(&mut db, undo);
        }
        self.aborted = false;
    }

    /// Execute one parsed statement.
    pub fn execute_parsed(&mut self, stmt: &Statement, positional: &[Value], named: Option<&ObjectMap>, opts: &ExecuteOptions) -> SqlResult<QueryResult> {
        match stmt {
            Statement::Begin => {
                if self.tx.is_none() {
                    self.tx = Some(Vec::new());
                    self.aborted = false;
                }
                return Ok(QueryResult::update("BEGIN", 0));
            }
            Statement::Commit => {
                if self.aborted {
                    self.do_rollback();
                    return Ok(QueryResult::update("ROLLBACK", 0));
                }
                self.tx = None;
                return Ok(QueryResult::update("COMMIT", 0));
            }
            Statement::Rollback => {
                self.do_rollback();
                return Ok(QueryResult::update("ROLLBACK", 0));
            }
            _ => {}
        }
        if self.aborted {
            return Err(SqlErr::new("25P02", "current transaction is aborted, commands ignored until end of transaction block"));
        }
        let params = Params { positional, named };
        let now = now_text();
        let mut db = self.db.db.lock().unwrap_or_else(|e| e.into_inner());
        let mut undo = Vec::new();
        match execute_statement(&mut db, stmt, &params, &mut undo, opts, &now) {
            Ok(r) => {
                if let Some(tx) = &mut self.tx {
                    tx.extend(undo);
                }
                Ok(r)
            }
            Err(e) => {
                apply_undo(&mut db, undo);
                if self.tx.is_some() {
                    self.aborted = true;
                }
                Err(e)
            }
        }
    }

    /// Execute a script; returns the result of every statement.
    pub fn execute_script(&mut self, sql: &str, positional: &[Value], named: Option<&ObjectMap>, opts: &ExecuteOptions) -> SqlResult<Vec<QueryResult>> {
        let stmts = match parser::parse_statements(sql) {
            Ok(s) => s,
            Err(e) => {
                if self.tx.is_some() {
                    self.aborted = true;
                }
                return Err(e);
            }
        };
        let mut out = Vec::new();
        for s in &stmts {
            out.push(self.execute_parsed(s, positional, named, opts)?);
        }
        Ok(out)
    }

    /// Execute SQL with named parameters (`:name` / `:#name`).
    pub fn execute_named(&mut self, sql: &str, named: &ObjectMap) -> Result<QueryResult> {
        let mut v = self.execute_script(sql, &[], Some(named), &ExecuteOptions::default())?;
        Ok(v.pop().unwrap_or_default())
    }
}

impl Drop for EmbeddedConnection {
    fn drop(&mut self) {
        self.do_rollback();
    }
}

impl Connection for EmbeddedConnection {
    fn execute_with(&mut self, sql: &str, params: &[Value], options: &ExecuteOptions) -> Result<QueryResult> {
        let mut v = self.execute_script(sql, params, None, options)?;
        Ok(v.pop().unwrap_or_else(|| QueryResult::update("", 0)))
    }
    fn begin(&mut self) -> Result<()> {
        self.execute_parsed(&Statement::Begin, &[], None, &ExecuteOptions::default())?;
        Ok(())
    }
    fn commit(&mut self) -> Result<()> {
        let r = self.execute_parsed(&Statement::Commit, &[], None, &ExecuteOptions::default())?;
        if r.command == "ROLLBACK" {
            return Err(sql_error("40000", "transaction was aborted by an earlier error and has been rolled back"));
        }
        Ok(())
    }
    fn rollback(&mut self) -> Result<()> {
        self.do_rollback();
        Ok(())
    }
    fn in_transaction(&self) -> bool {
        self.tx.is_some()
    }
}

/// Data source for an embedded database (`jdbc:rustsql:mem:<name>`).
#[derive(Clone, Debug)]
pub struct EmbeddedDataSource {
    name: String,
}

impl EmbeddedDataSource {
    /// Data source for the named shared database (created on first use).
    pub fn new(name: &str) -> Self {
        database(name);
        EmbeddedDataSource { name: name.to_string() }
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Open a concrete connection.
    pub fn connect(&self) -> EmbeddedConnection {
        EmbeddedConnection::open(&self.name)
    }
}

impl DataSource for EmbeddedDataSource {
    fn get_connection(&self) -> Result<Box<dyn Connection>> {
        Ok(Box::new(self.connect()))
    }
    fn description(&self) -> String {
        format!("jdbc:rustsql:mem:{}", self.name)
    }
    fn key(&self) -> String {
        format!("rustsql:mem:{}", self.name)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod routine_tests;
