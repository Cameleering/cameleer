//! The DataSource SPI (the equivalent of `javax.sql.DataSource` / `java.sql.Connection`).
//!
//! A [`DataSource`] hands out [`Connection`]s. Connections execute SQL with positional
//! `?` parameters and report a [`QueryResult`]. Data sources are bound in the Camel
//! registry by name ([`bind_datasource`]) and referenced from endpoints as
//! `dataSource=#name`; they can also be created from a JDBC-style URL
//! ([`datasource_from_url`]):
//!
//! * `jdbc:rustsql:mem:<name>` — the embedded in-memory [rustsql](crate::engine) engine
//!   (one shared database per name),
//! * `jdbc:postgresql://host:port/db?user=..&password=..` — the PostgreSQL wire protocol
//!   client ([`crate::pg`]).

use std::sync::{Arc, Once};

use cameleer_core::context::CamelContext;
use cameleer_core::error::{register_kind_parent, CamelError, Result};
use cameleer_core::value::{ObjectMap, Value};

/// Exception kinds used by this crate (Java `java.sql` class names).
pub mod kinds {
    /// Base kind of every database error.
    pub const SQL_EXCEPTION: &str = "SQLException";
    /// Syntax errors and unknown tables/columns (SQLState class `42`).
    pub const SQL_SYNTAX_ERROR: &str = "SQLSyntaxErrorException";
    /// Constraint violations (SQLState class `23`).
    pub const SQL_INTEGRITY_CONSTRAINT_VIOLATION: &str = "SQLIntegrityConstraintViolationException";
    /// Data errors such as division by zero, bad casts or a non-unique `SelectOne` result.
    pub const SQL_DATA: &str = "SQLDataException";
    /// Transaction state errors (SQLState classes `25` / `40`).
    pub const SQL_TRANSACTION_ROLLBACK: &str = "SQLTransactionRollbackException";
    /// Connection failures and authentication errors (SQLState classes `08` / `28`).
    pub const SQL_NON_TRANSIENT_CONNECTION: &str = "SQLNonTransientConnectionException";
}

static KINDS: Once = Once::new();

/// Register the SQL exception hierarchy (idempotent).
pub fn register_sql_kinds() {
    KINDS.call_once(|| {
        register_kind_parent(kinds::SQL_EXCEPTION, cameleer_core::error::kinds::EXCEPTION);
        for k in [
            kinds::SQL_SYNTAX_ERROR,
            kinds::SQL_INTEGRITY_CONSTRAINT_VIOLATION,
            kinds::SQL_DATA,
            kinds::SQL_TRANSACTION_ROLLBACK,
            kinds::SQL_NON_TRANSIENT_CONNECTION,
        ] {
            register_kind_parent(k, kinds::SQL_EXCEPTION);
        }
    });
}

/// The exception kind for a SQLState code.
pub fn kind_for_sqlstate(state: &str) -> &'static str {
    match state.get(..2).unwrap_or("") {
        "42" => kinds::SQL_SYNTAX_ERROR,
        "23" => kinds::SQL_INTEGRITY_CONSTRAINT_VIOLATION,
        "22" => kinds::SQL_DATA,
        "25" | "40" => kinds::SQL_TRANSACTION_ROLLBACK,
        "08" | "28" => kinds::SQL_NON_TRANSIENT_CONNECTION,
        _ => kinds::SQL_EXCEPTION,
    }
}

/// Build a SQL error: the kind follows the SQLState class and the message ends with
/// `[SQLState: xxxxx]`.
pub fn sql_error(state: &str, message: impl Into<String>) -> CamelError {
    register_sql_kinds();
    CamelError::new(kind_for_sqlstate(state), format!("{} [SQLState: {}]", message.into(), state))
}

/// The SQLState of an error created by [`sql_error`], if any.
pub fn sqlstate_of(e: &CamelError) -> Option<String> {
    let m = e.message.rfind("[SQLState: ")?;
    let rest = &e.message[m + 11..];
    rest.strip_suffix(']').map(|s| s.to_string())
}

/// Generic column type of a result column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnType {
    Integer,
    BigInt,
    Text,
    Boolean,
    Double,
    Decimal,
    Timestamp,
    Bytes,
    Unknown,
}

impl ColumnType {
    /// Infer a column type from a value.
    pub fn of_value(v: &Value) -> ColumnType {
        match v {
            Value::Int(_) => ColumnType::BigInt,
            Value::Float(_) => ColumnType::Double,
            Value::Bool(_) => ColumnType::Boolean,
            Value::String(_) => ColumnType::Text,
            Value::Bytes(_) => ColumnType::Bytes,
            _ => ColumnType::Unknown,
        }
    }
}

/// The result of executing one statement.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryResult {
    /// Result column names (empty for statements that return no rows).
    pub columns: Vec<String>,
    /// Column types (same length as `columns`).
    pub column_types: Vec<ColumnType>,
    /// Result rows.
    pub rows: Vec<Vec<Value>>,
    /// Rows affected by INSERT/UPDATE/DELETE (or returned by SELECT).
    pub update_count: i64,
    /// Generated keys (auto increment / serial columns) of inserted rows.
    pub generated_keys: Vec<ObjectMap>,
    /// True if the statement produces a result set (SELECT, `... RETURNING`).
    pub is_query: bool,
    /// The command tag (`SELECT`, `INSERT`, `CREATE TABLE`...).
    pub command: String,
}

impl QueryResult {
    /// A result of a statement without rows.
    pub fn update(command: &str, count: i64) -> Self {
        QueryResult { update_count: count, command: command.to_string(), ..Default::default() }
    }

    /// The rows as column name → value maps.
    pub fn rows_as_maps(&self) -> Vec<ObjectMap> {
        self.rows.iter().map(|r| self.columns.iter().cloned().zip(r.iter().cloned()).collect()).collect()
    }

    /// The rows as `Value::Object`s.
    pub fn rows_as_values(&self) -> Vec<Value> {
        self.rows_as_maps().into_iter().map(Value::Object).collect()
    }

    /// First column of the first row.
    pub fn scalar(&self) -> Option<&Value> {
        self.rows.first().and_then(|r| r.first())
    }
}

/// Extra options for [`Connection::execute_with`].
#[derive(Clone, Debug, Default)]
pub struct ExecuteOptions {
    /// Ask for generated keys (JDBC `RETURN_GENERATED_KEYS`).
    pub return_generated_keys: bool,
    /// Maximum number of rows to return (0 = all).
    pub max_rows: usize,
}

/// A database connection (`java.sql.Connection`). Connections are in auto-commit mode
/// until [`Connection::begin`] is called.
pub trait Connection: Send {
    /// Execute SQL with positional `?` parameters.
    fn execute(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.execute_with(sql, params, &ExecuteOptions::default())
    }
    /// Execute SQL with positional `?` parameters and options.
    fn execute_with(&mut self, sql: &str, params: &[Value], options: &ExecuteOptions) -> Result<QueryResult>;
    /// Start a transaction (leave auto-commit mode).
    fn begin(&mut self) -> Result<()>;
    /// Commit the current transaction.
    fn commit(&mut self) -> Result<()>;
    /// Roll back the current transaction.
    fn rollback(&mut self) -> Result<()>;
    /// Whether a transaction is open.
    fn in_transaction(&self) -> bool;
    /// Whether the connection is still usable (used by pools).
    fn is_valid(&self) -> bool {
        true
    }
}

/// A factory of connections (`javax.sql.DataSource`).
pub trait DataSource: Send + Sync {
    /// Open (or borrow from a pool) a connection.
    fn get_connection(&self) -> Result<Box<dyn Connection>>;
    /// A display name / url for logging.
    fn description(&self) -> String {
        "DataSource".to_string()
    }
    /// Identity used to bind a transaction to the data source (two handles with the same
    /// key share transactions). Defaults to the address of `self`.
    fn key(&self) -> String {
        format!("ds@{:x}", self as *const Self as *const () as usize)
    }
}

/// Create a data source from a JDBC-style url (`jdbc:rustsql:mem:name`,
/// `jdbc:postgresql://host:port/db?user=&password=`). The `jdbc:` prefix is optional.
pub fn datasource_from_url(url: &str) -> Result<Arc<dyn DataSource>> {
    datasource_from_url_with(url, None, None)
}

/// Like [`datasource_from_url`] with explicit credentials (overriding the url's).
pub fn datasource_from_url_with(url: &str, user: Option<&str>, password: Option<&str>) -> Result<Arc<dyn DataSource>> {
    let u = url.trim();
    let u = u.strip_prefix("jdbc:").unwrap_or(u);
    if let Some(name) = u.strip_prefix("rustsql:mem:").or_else(|| u.strip_prefix("rustsql:")) {
        let name = name.split(';').next().unwrap_or("").trim();
        if name.is_empty() {
            return Err(CamelError::illegal_argument(format!("Missing database name in url: {}", url)));
        }
        return Ok(Arc::new(crate::engine::EmbeddedDataSource::new(name)));
    }
    if u.starts_with("postgresql:") || u.starts_with("postgres:") {
        let mut cfg = crate::pg::PgConfig::from_url(u)?;
        if let Some(us) = user {
            cfg.user = us.to_string();
        }
        if let Some(p) = password {
            cfg.password = Some(p.to_string());
        }
        return Ok(Arc::new(crate::pg::PgDataSource::new(cfg)));
    }
    Err(CamelError::illegal_argument(format!(
        "Unsupported datasource url: {} (supported: jdbc:rustsql:mem:<name>, jdbc:postgresql://host:port/db)",
        url
    )))
}

/// Bind a data source in the context registry under `name`.
pub fn bind_datasource(ctx: &CamelContext, name: &str, ds: Arc<dyn DataSource>) {
    ctx.registry().bind(name.trim_start_matches('#'), ds);
}

/// Look up a data source by registry name (a leading `#` is ignored). Accepts beans bound
/// as `Arc<dyn DataSource>`, [`crate::engine::EmbeddedDataSource`], [`crate::pg::PgDataSource`]
/// or a url `String`.
pub fn lookup_datasource(ctx: &CamelContext, name: &str) -> Result<Arc<dyn DataSource>> {
    let name = name.trim().trim_start_matches('#');
    let name = name.strip_prefix("bean:").unwrap_or(name);
    let reg = ctx.registry();
    if let Some(ds) = reg.lookup::<Arc<dyn DataSource>>(name) {
        return Ok((*ds).clone());
    }
    if let Some(ds) = reg.lookup::<crate::engine::EmbeddedDataSource>(name) {
        return Ok(Arc::new((*ds).clone()));
    }
    if let Some(ds) = reg.lookup::<crate::pg::PgDataSource>(name) {
        return Ok(ds as Arc<dyn DataSource>);
    }
    if let Some(url) = reg.lookup::<String>(name) {
        let ds = datasource_from_url(&url)?;
        return Ok(ds);
    }
    Err(CamelError::no_such_bean(name))
}

