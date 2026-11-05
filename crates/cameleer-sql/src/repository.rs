//! JDBC-backed repositories for the idempotent consumer and the aggregator.

use std::sync::Arc;

use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::{Exchange, ExchangePattern};
use cameleer_core::processor::errors::IdempotentRepository;
use cameleer_core::processor::routing::AggregationRepository;
use cameleer_core::value::{ObjectMap, Value};

use crate::datasource::DataSource;
use crate::pg::crypto::{b64, b64_decode};
use crate::transaction::with_connection;

fn valid_identifier(name: &str) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Err(CamelError::illegal_argument(format!("Invalid table name: {}", name)));
    }
    Ok(())
}

/// Idempotent repository stored in a table (Camel `JdbcMessageIdRepository`).
///
/// Table (created if missing): `CAMEL_MESSAGEPROCESSED (processorName VARCHAR(255),
/// messageId VARCHAR(100), createdAt TIMESTAMP, PRIMARY KEY (processorName, messageId))`.
/// Statements join a thread-bound JDBC transaction when one is active.
pub struct JdbcIdempotentRepository {
    ds: Arc<dyn DataSource>,
    processor_name: String,
    table: String,
}

impl JdbcIdempotentRepository {
    /// Create the repository (and the `CAMEL_MESSAGEPROCESSED` table if missing).
    pub fn new(ds: Arc<dyn DataSource>, processor_name: &str) -> Result<Self> {
        Self::with_table(ds, processor_name, "CAMEL_MESSAGEPROCESSED")
    }

    /// Use a custom table name.
    pub fn with_table(ds: Arc<dyn DataSource>, processor_name: &str, table: &str) -> Result<Self> {
        valid_identifier(table)?;
        let r = JdbcIdempotentRepository { ds, processor_name: processor_name.to_string(), table: table.to_string() };
        r.create_table()?;
        Ok(r)
    }

    /// Create the table if it does not exist.
    pub fn create_table(&self) -> Result<()> {
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {} (processorName VARCHAR(255) NOT NULL, messageId VARCHAR(100) NOT NULL, createdAt TIMESTAMP, PRIMARY KEY (processorName, messageId))",
            self.table
        );
        with_connection(&self.ds, |c| c.execute(&sql, &[])).map(|_| ())
    }

    pub fn processor_name(&self) -> &str {
        &self.processor_name
    }

    fn query_count(&self, key: &str) -> Result<i64> {
        let sql = format!("SELECT COUNT(*) FROM {} WHERE processorName = ? AND messageId = ?", self.table);
        let r = with_connection(&self.ds, |c| c.execute(&sql, &[Value::from(self.processor_name.as_str()), Value::from(key)]))?;
        Ok(r.scalar().and_then(|v| v.as_i64()).unwrap_or(0))
    }

    fn try_add(&self, key: &str) -> Result<bool> {
        if self.query_count(key)? > 0 {
            return Ok(false);
        }
        let sql = format!("INSERT INTO {} (processorName, messageId, createdAt) VALUES (?, ?, CURRENT_TIMESTAMP)", self.table);
        with_connection(&self.ds, |c| c.execute(&sql, &[Value::from(self.processor_name.as_str()), Value::from(key)]))?;
        Ok(true)
    }

    fn try_remove(&self, key: &str) -> Result<bool> {
        let sql = format!("DELETE FROM {} WHERE processorName = ? AND messageId = ?", self.table);
        let r = with_connection(&self.ds, |c| c.execute(&sql, &[Value::from(self.processor_name.as_str()), Value::from(key)]))?;
        Ok(r.update_count > 0)
    }

    /// Number of keys stored for this processor.
    pub fn size(&self) -> Result<i64> {
        let sql = format!("SELECT COUNT(*) FROM {} WHERE processorName = ?", self.table);
        let r = with_connection(&self.ds, |c| c.execute(&sql, &[Value::from(self.processor_name.as_str())]))?;
        Ok(r.scalar().and_then(|v| v.as_i64()).unwrap_or(0))
    }
}

fn log_err<T>(what: &str, r: Result<T>, default: T) -> T {
    r.unwrap_or_else(|e| {
        cameleer_core::camel_warn!("JdbcIdempotentRepository", "Error during {}: {}", what, e);
        default
    })
}

impl IdempotentRepository for JdbcIdempotentRepository {
    fn add(&self, key: &str) -> bool {
        match self.try_add(key) {
            Ok(b) => b,
            Err(e) if e.kind == crate::datasource::kinds::SQL_INTEGRITY_CONSTRAINT_VIOLATION => false,
            Err(e) => log_err("add", Err(e), false),
        }
    }
    fn contains(&self, key: &str) -> bool {
        log_err("contains", self.query_count(key).map(|n| n > 0), false)
    }
    fn remove(&self, key: &str) -> bool {
        log_err("remove", self.try_remove(key), false)
    }
    fn clear(&self) {
        let sql = format!("DELETE FROM {} WHERE processorName = ?", self.table);
        log_err("clear", with_connection(&self.ds, |c| c.execute(&sql, &[Value::from(self.processor_name.as_str())])).map(|_| ()), ());
    }
}

// ---------------------------------------------------------------------------
// exchange serialization
// ---------------------------------------------------------------------------

fn enc(v: &Value) -> Value {
    match v {
        Value::Bytes(b) => Value::object([("$bytes", Value::String(b64(b)))]),
        Value::Array(a) => Value::Array(a.iter().map(enc).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), enc(v))).collect()),
        other => other.clone(),
    }
}

fn dec(v: Value) -> Value {
    match v {
        Value::Object(o) => {
            if o.len() == 1 {
                if let Some(Value::String(s)) = o.get("$bytes") {
                    if let Some(b) = b64_decode(s) {
                        return Value::Bytes(b);
                    }
                }
            }
            Value::Object(o.into_iter().map(|(k, v)| (k, dec(v))).collect())
        }
        Value::Array(a) => Value::Array(a.into_iter().map(dec).collect()),
        other => other,
    }
}

/// Serialize an exchange snapshot (id, pattern, body, headers, properties) as JSON.
pub fn exchange_to_json(ex: &Exchange) -> String {
    let headers: ObjectMap = ex.message.headers.iter().map(|(k, v)| (k.to_string(), enc(v))).collect();
    let props: ObjectMap = ex.properties.iter().map(|(k, v)| (k.to_string(), enc(v))).collect();
    let mut m = ObjectMap::new();
    m.insert("exchangeId".to_string(), Value::String(ex.exchange_id.to_string()));
    m.insert("pattern".to_string(), Value::String(format!("{:?}", ex.pattern)));
    m.insert("body".to_string(), enc(&ex.message.body));
    m.insert("headers".to_string(), Value::Object(headers));
    m.insert("properties".to_string(), Value::Object(props));
    if let Some(f) = &ex.from_endpoint {
        m.insert("fromEndpoint".to_string(), Value::String(f.to_string()));
    }
    if let Some(r) = &ex.from_route_id {
        m.insert("fromRouteId".to_string(), Value::String(r.to_string()));
    }
    Value::Object(m).to_json()
}

/// Restore an exchange from [`exchange_to_json`] output.
pub fn exchange_from_json(text: &str, ctx: Option<CamelContext>) -> Result<Exchange> {
    let v = cameleer_core::json::parse(text)?;
    let Value::Object(mut m) = v else {
        return Err(CamelError::new(cameleer_core::error::kinds::TYPE_CONVERSION, "Stored exchange is not a JSON object"));
    };
    let mut ex = Exchange::new(ctx);
    if let Some(Value::String(id)) = m.remove("exchangeId") {
        ex.exchange_id = id.into();
    }
    if let Some(Value::String(p)) = m.remove("pattern") {
        ex.pattern = ExchangePattern::parse(&p).unwrap_or_default();
    }
    ex.message.body = dec(m.remove("body").unwrap_or(Value::Null));
    if let Some(Value::Object(h)) = m.remove("headers") {
        for (k, v) in h {
            ex.message.headers.set(k, dec(v));
        }
    }
    if let Some(Value::Object(p)) = m.remove("properties") {
        for (k, v) in p {
            ex.properties.insert(k, dec(v));
        }
    }
    if let Some(Value::String(f)) = m.remove("fromEndpoint") {
        ex.from_endpoint = Some(f.into());
    }
    if let Some(Value::String(r)) = m.remove("fromRouteId") {
        ex.from_route_id = Some(r.into());
    }
    Ok(ex)
}

/// Aggregation repository stored in two tables (Camel `JdbcAggregationRepository`):
/// `<name>` holds the groups in progress and `<name>_completed` the completed exchanges
/// until they are confirmed. Exchanges are stored as JSON snapshots
/// ([`exchange_to_json`]): body, headers and properties survive, on-completion callbacks do not.
///
/// Recovery: [`JdbcAggregationRepository::scan`] lists completed but unconfirmed exchange ids
/// and [`JdbcAggregationRepository::recover`] loads one (no background recovery task).
pub struct JdbcAggregationRepository {
    ds: Arc<dyn DataSource>,
    table: String,
    ctx: Option<WeakContext>,
}

impl JdbcAggregationRepository {
    /// Create the repository (tables `<name>` and `<name>_completed` are created if missing).
    pub fn new(ctx: &CamelContext, ds: Arc<dyn DataSource>, repository_name: &str) -> Result<Self> {
        valid_identifier(repository_name)?;
        let r = JdbcAggregationRepository { ds, table: repository_name.to_string(), ctx: Some(ctx.downgrade()) };
        r.create_tables()?;
        Ok(r)
    }

    /// Create the tables if they do not exist.
    pub fn create_tables(&self) -> Result<()> {
        let t = &self.table;
        let a = format!("CREATE TABLE IF NOT EXISTS {} (id VARCHAR(255) NOT NULL PRIMARY KEY, exchange TEXT NOT NULL, version BIGINT NOT NULL, seq BIGINT NOT NULL)", t);
        let b = format!("CREATE TABLE IF NOT EXISTS {}_completed (id VARCHAR(255) NOT NULL PRIMARY KEY, exchange TEXT NOT NULL, version BIGINT NOT NULL)", t);
        with_connection(&self.ds, |c| {
            c.execute(&a, &[])?;
            c.execute(&b, &[])
        })
        .map(|_| ())
    }

    fn context(&self) -> Option<CamelContext> {
        self.ctx.as_ref().and_then(|c| c.upgrade())
    }

    fn load(&self, table: &str, id: &str) -> Result<Option<Exchange>> {
        let sql = format!("SELECT exchange FROM {} WHERE id = ?", table);
        let r = with_connection(&self.ds, |c| c.execute(&sql, &[Value::from(id)]))?;
        match r.scalar() {
            Some(Value::String(s)) => Ok(Some(exchange_from_json(s, self.context())?)),
            Some(Value::Bytes(b)) => Ok(Some(exchange_from_json(&String::from_utf8_lossy(b), self.context())?)),
            _ => Ok(None),
        }
    }

    fn try_add(&self, key: &str, ex: &Exchange) -> Result<Option<Exchange>> {
        let old = self.load(&self.table, key)?;
        let json = exchange_to_json(ex);
        let t = &self.table;
        if old.is_some() {
            let sql = format!("UPDATE {} SET exchange = ?, version = version + 1 WHERE id = ?", t);
            with_connection(&self.ds, |c| c.execute(&sql, &[Value::String(json), Value::from(key)]))?;
        } else {
            let seq_sql = format!("SELECT COALESCE(MAX(seq), 0) + 1 FROM {}", t);
            let ins = format!("INSERT INTO {} (id, exchange, version, seq) VALUES (?, ?, 1, ?)", t);
            with_connection(&self.ds, |c| {
                let seq = c.execute(&seq_sql, &[])?.scalar().and_then(|v| v.as_i64()).unwrap_or(1);
                c.execute(&ins, &[Value::from(key), Value::String(json), Value::Int(seq)])
            })?;
        }
        Ok(old)
    }

    fn try_remove(&self, key: &str) -> Result<Option<Exchange>> {
        let Some(ex) = self.load(&self.table, key)? else { return Ok(None) };
        let t = &self.table;
        let del = format!("DELETE FROM {} WHERE id = ?", t);
        let del_c = format!("DELETE FROM {}_completed WHERE id = ?", t);
        let ins = format!("INSERT INTO {}_completed (id, exchange, version) VALUES (?, ?, 1)", t);
        let json = exchange_to_json(&ex);
        let id = ex.exchange_id.to_string();
        with_connection(&self.ds, |c| {
            c.execute(&del, &[Value::from(key)])?;
            c.execute(&del_c, &[Value::from(id.as_str())])?;
            c.execute(&ins, &[Value::from(id.as_str()), Value::String(json)])
        })?;
        Ok(Some(ex))
    }

    fn try_keys(&self) -> Result<Vec<String>> {
        let sql = format!("SELECT id FROM {} ORDER BY seq", self.table);
        let r = with_connection(&self.ds, |c| c.execute(&sql, &[]))?;
        Ok(r.rows.into_iter().filter_map(|mut row| if row.is_empty() { None } else { Some(row.remove(0).to_string()) }).collect())
    }

    /// Exchange ids of completed exchanges that were not confirmed yet.
    pub fn scan(&self) -> Result<Vec<String>> {
        let sql = format!("SELECT id FROM {}_completed ORDER BY id", self.table);
        let r = with_connection(&self.ds, |c| c.execute(&sql, &[]))?;
        Ok(r.rows.into_iter().filter_map(|mut row| if row.is_empty() { None } else { Some(row.remove(0).to_string()) }).collect())
    }

    /// Load a completed (unconfirmed) exchange for recovery.
    pub fn recover(&self, exchange_id: &str) -> Result<Option<Exchange>> {
        self.load(&format!("{}_completed", self.table), exchange_id)
    }
}

fn warn(what: &str, e: &CamelError) {
    cameleer_core::camel_warn!("JdbcAggregationRepository", "Error during {}: {}", what, e);
}

impl AggregationRepository for JdbcAggregationRepository {
    fn add(&self, key: &str, exchange: Exchange) -> Option<Exchange> {
        self.try_add(key, &exchange).unwrap_or_else(|e| {
            warn("add", &e);
            None
        })
    }
    fn get(&self, key: &str) -> Option<Exchange> {
        self.load(&self.table, key).unwrap_or_else(|e| {
            warn("get", &e);
            None
        })
    }
    fn remove(&self, key: &str) -> Option<Exchange> {
        self.try_remove(key).unwrap_or_else(|e| {
            warn("remove", &e);
            None
        })
    }
    fn confirm(&self, exchange_id: &str) {
        let sql = format!("DELETE FROM {}_completed WHERE id = ?", self.table);
        if let Err(e) = with_connection(&self.ds, |c| c.execute(&sql, &[Value::from(exchange_id)])) {
            warn("confirm", &e);
        }
    }
    fn keys(&self) -> Vec<String> {
        self.try_keys().unwrap_or_else(|e| {
            warn("keys", &e);
            Vec::new()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip() {
        let mut ex = Exchange::new(None);
        ex.pattern = ExchangePattern::InOut;
        ex.set_body(Value::Bytes(vec![0, 1, 255]));
        ex.set_header("Foo", Value::Array(vec![Value::Int(1), Value::from("x")]));
        ex.set_property("p", 1.5);
        let back = exchange_from_json(&exchange_to_json(&ex), None).unwrap();
        assert_eq!(back.exchange_id, ex.exchange_id);
        assert_eq!(back.pattern, ExchangePattern::InOut);
        assert_eq!(back.body(), &Value::Bytes(vec![0, 1, 255]));
        assert_eq!(back.header("foo"), ex.header("Foo"));
        assert_eq!(back.property("p"), Some(&Value::Float(1.5)));
    }
}
