//! The `jdbc` component (Camel `camel-jdbc`): executes the SQL in the message body.
//!
//! `jdbc:dataSourceName?outputType=SelectList|SelectOne|StreamList&useHeadersAsParameters=true&readSize=10&statement.maxRows=10&resetAutoCommit=true`
//!
//! With `useHeadersAsParameters=true`, `:?name` placeholders are filled from the
//! `CamelJdbcParameters` header map, then from the message headers.
//! Out headers: `CamelJdbcRowCount`, `CamelJdbcColumnNames` (SELECT), `CamelJdbcUpdateCount`
//! (other statements), `CamelGeneratedKeysRows` / `CamelGeneratedKeysRowCount` when the
//! `CamelRetrieveGeneratedKeys` header is true.

use std::any::Any;
use std::sync::Arc;

use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

use crate::datasource::{lookup_datasource, DataSource, ExecuteOptions};
use crate::params::{bind, parse_sql};
use crate::sql::{select_body, OutputType};
use crate::transaction::with_connection;

/// Header names of the jdbc component.
pub mod headers {
    pub const JDBC_ROW_COUNT: &str = "CamelJdbcRowCount";
    pub const JDBC_UPDATE_COUNT: &str = "CamelJdbcUpdateCount";
    pub const JDBC_COLUMN_NAMES: &str = "CamelJdbcColumnNames";
    pub const JDBC_PARAMETERS: &str = "CamelJdbcParameters";
    pub const RETRIEVE_GENERATED_KEYS: &str = "CamelRetrieveGeneratedKeys";
    pub const GENERATED_COLUMNS: &str = "CamelGeneratedColumns";
    pub const GENERATED_KEYS_ROW_COUNT: &str = "CamelGeneratedKeysRowCount";
    pub const GENERATED_KEYS_ROWS: &str = "CamelGeneratedKeysRows";
}

/// The `jdbc` component.
#[derive(Default)]
pub struct JdbcComponent {
    default_datasource: Option<Arc<dyn DataSource>>,
}

impl JdbcComponent {
    pub fn new() -> Self {
        Self::default()
    }
    /// Use this data source for `jdbc:dataSource` (Camel's component-level `dataSource`).
    pub fn with_datasource(ds: Arc<dyn DataSource>) -> Self {
        JdbcComponent { default_datasource: Some(ds) }
    }
}

/// jdbc endpoint configuration.
#[derive(Clone)]
pub struct JdbcConfig {
    pub datasource: Arc<dyn DataSource>,
    pub output_type: OutputType,
    pub use_headers_as_parameters: bool,
    pub allow_named_parameters: bool,
    pub reset_auto_commit: bool,
    pub transacted: bool,
    pub read_size: usize,
}

impl Component for JdbcComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let name = remaining.trim();
        if name.is_empty() {
            return Err(CamelError::illegal_argument("A data source name is required: jdbc:dataSourceName"));
        }
        let datasource = match lookup_datasource(ctx, name) {
            Ok(d) => d,
            Err(e) => match (&self.default_datasource, name) {
                (Some(d), "dataSource" | "default") => d.clone(),
                _ => return Err(e),
            },
        };
        let output_type = match params.take("outputType") {
            Some(s) => OutputType::parse(&s)?,
            None => OutputType::SelectList,
        };
        let mut statement = params.take_prefixed("statement.");
        let mut read_size = params.take_i64("readSize", 0)?.max(0) as usize;
        if let Some(m) = statement.remove("maxRows") {
            let m = m.parse::<i64>().map_err(|_| CamelError::illegal_argument(format!("Invalid statement.maxRows: {}", m)))?.max(0) as usize;
            if m > 0 && (read_size == 0 || m < read_size) {
                read_size = m;
            }
        }
        let _ = params.take_prefixed("parameters.");
        for k in ["outputClass", "beanRowMapper", "useJDBC4ColumnNameAndLabelSemantics", "useGetBytesForBlob", "prepareStatementStrategy", "lazyStartProducer", "connectionStrategy"] {
            params.take(k);
        }
        let cfg = JdbcConfig {
            datasource,
            output_type,
            use_headers_as_parameters: params.take_bool("useHeadersAsParameters", false)?,
            allow_named_parameters: params.take_bool("allowNamedParameters", true)?,
            reset_auto_commit: params.take_bool("resetAutoCommit", true)?,
            transacted: params.take_bool("transacted", false)?,
            read_size,
        };
        Ok(Arc::new(JdbcEndpoint { uri: uri.to_string(), cfg: Arc::new(cfg) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `jdbc:` endpoint (producer only).
pub struct JdbcEndpoint {
    uri: String,
    cfg: Arc<JdbcConfig>,
}

impl JdbcEndpoint {
    pub fn config(&self) -> &JdbcConfig {
        &self.cfg
    }
}

impl Endpoint for JdbcEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(JdbcProducer { cfg: self.cfg.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct JdbcProducer {
    cfg: Arc<JdbcConfig>,
}

impl Service for JdbcProducer {}

impl Processor for JdbcProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let cfg = &self.cfg;
        let sql = match ex.body() {
            Value::Null => return Err(CamelError::illegal_argument("The message body must contain the SQL to execute")),
            v => v.to_string(),
        };
        if sql.trim().is_empty() {
            return Err(CamelError::illegal_argument("The message body must contain the SQL to execute"));
        }
        let (sql, values) = if cfg.use_headers_as_parameters {
            let parsed = parse_sql(&sql, "", ":?", cfg.allow_named_parameters)?;
            let map = ex.header(headers::JDBC_PARAMETERS).and_then(|v| v.as_object()).cloned();
            let positional = match ex.header(headers::JDBC_PARAMETERS) {
                Some(Value::Array(a)) => a.clone(),
                _ => Vec::new(),
            };
            bind(&parsed, ex, map.as_ref(), positional, 0)?
        } else {
            (sql, Vec::new())
        };
        let want_keys = ex.header(headers::RETRIEVE_GENERATED_KEYS).map(|v| v.is_truthy()).unwrap_or(false);
        let opts = ExecuteOptions { return_generated_keys: want_keys, max_rows: cfg.read_size };
        let r = with_connection(&cfg.datasource, |c| c.execute_with(&sql, &values, &opts))?;
        if r.is_query {
            ex.set_header(headers::JDBC_ROW_COUNT, r.rows.len() as i64);
            ex.set_header(headers::JDBC_COLUMN_NAMES, Value::Array(r.columns.iter().map(|c| Value::String(c.clone())).collect()));
            let body = select_body(&r, cfg.output_type)?;
            ex.set_body(body.unwrap_or(Value::Null));
        } else {
            ex.set_header(headers::JDBC_UPDATE_COUNT, r.update_count);
            if want_keys {
                let rows: Vec<Value> = r.generated_keys.iter().cloned().map(Value::Object).collect();
                ex.set_header(headers::GENERATED_KEYS_ROW_COUNT, rows.len() as i64);
                ex.set_header(headers::GENERATED_KEYS_ROWS, Value::Array(rows));
            }
        }
        Ok(())
    }
}
