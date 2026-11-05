//! The `sql` component (Camel `cameleer-sql`): run SQL queries with named/positional parameters
//! as a producer, or poll a query as a consumer.
//!
//! `sql:select * from orders where status = :#status?dataSource=#ds&outputType=SelectList`
//!
//! Producer options: `dataSource`, `outputType` (`SelectList`, `SelectOne`, `StreamList`),
//! `outputHeader`, `noop`, `batch`, `useMessageBodyForSql`, `parametersCount`, `placeholder`
//! (default `#`), `usePlaceholder`, `separator` (default `,`), `allowNamedParameters`,
//! `template.maxRows`, `outputClass` (accepted, rows stay maps).
//! Consumer options: `useIterator`, `onConsume`, `onConsumeFailed`, `onConsumeBatchComplete`,
//! `maxMessagesPerPoll`, `routeEmptyResultSet`, `transacted`, `breakBatchOnConsumeFail`,
//! `expectedUpdateCount` and the scheduled poll options (`delay` default 500 ms,
//! `initialDelay` default 1000 ms, ...).
//!
//! The query may also be loaded from a file: `sql:file:queries/select.sql`,
//! `sql:classpath:sql/select.sql`.

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use cameleer_core::component::scheduled::{ScheduledPollConfig, ScheduledPollConsumer};
use cameleer_core::component::support::process_consumer_exchange;
use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::{props, Exchange};
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::{ObjectMap, Value};

use crate::datasource::{kinds, lookup_datasource, sql_error, DataSource, ExecuteOptions, QueryResult};
use crate::params::{bind, parse_sql, positional_values, ParsedSql};
use crate::transaction::{with_connection, BoundTransaction};

/// Header and property names of the sql component.
pub mod headers {
    pub const SQL_QUERY: &str = "CamelSqlQuery";
    pub const SQL_UPDATE_COUNT: &str = "CamelSqlUpdateCount";
    pub const SQL_ROW_COUNT: &str = "CamelSqlRowCount";
    pub const SQL_RETRIEVE_GENERATED_KEYS: &str = "CamelSqlRetrieveGeneratedKeys";
    pub const SQL_GENERATED_COLUMNS: &str = "CamelSqlGeneratedColumns";
    pub const SQL_GENERATED_KEYS_ROW_COUNT: &str = "CamelSqlGeneratedKeysRowCount";
    pub const SQL_GENERATED_KEYS_DATA: &str = "CamelSqlGeneratedKeysRows";
    pub const SQL_PARAMETERS: &str = "CamelSqlParameters";
}

/// Result shape of a SELECT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputType {
    SelectList,
    SelectOne,
    StreamList,
}

impl OutputType {
    pub fn parse(s: &str) -> Result<OutputType> {
        match s {
            "SelectList" => Ok(OutputType::SelectList),
            "SelectOne" => Ok(OutputType::SelectOne),
            "StreamList" => Ok(OutputType::StreamList),
            other => Err(CamelError::illegal_argument(format!("Invalid outputType: {} (SelectList, SelectOne, StreamList)", other))),
        }
    }
}

/// Load a query from `file:` / `classpath:` resources, otherwise return it as is.
pub fn resolve_query_text(q: &str) -> Result<String> {
    let q = q.trim();
    let (path, classpath) = if let Some(p) = q.strip_prefix("file:") {
        (p, false)
    } else if let Some(p) = q.strip_prefix("classpath:") {
        (p, true)
    } else if let Some(p) = q.strip_prefix("resource:") {
        (p.strip_prefix("classpath:").or_else(|| p.strip_prefix("file:")).unwrap_or(p), true)
    } else {
        return Ok(q.to_string());
    };
    let mut candidates = vec![std::path::PathBuf::from(path)];
    if classpath {
        let p = path.trim_start_matches('/');
        for base in ["src/main/resources", "resources", "src/test/resources"] {
            candidates.push(std::path::Path::new(base).join(p));
        }
    }
    for c in &candidates {
        if let Ok(text) = std::fs::read_to_string(c) {
            // strip comment-only lines (Camel ignores `--` comment lines in query files)
            let lines: Vec<&str> = text.lines().filter(|l| !l.trim_start().starts_with("--")).collect();
            return Ok(lines.join("\n").trim().trim_end_matches(';').to_string());
        }
    }
    Err(CamelError::new(cameleer_core::error::kinds::FILE_NOT_FOUND, format!("Cannot load SQL query from {}", q)))
}

/// Find the data source for an endpoint: `dataSource=#name`, a component default, or the only
/// data source in the registry (autowiring).
pub fn resolve_endpoint_datasource(ctx: &CamelContext, option: Option<String>, default: Option<&Arc<dyn DataSource>>) -> Result<Arc<dyn DataSource>> {
    if let Some(name) = option {
        let name = name.trim();
        if name.starts_with("jdbc:") {
            return crate::datasource::datasource_from_url(name);
        }
        return lookup_datasource(ctx, name);
    }
    if let Some(d) = default {
        return Ok(d.clone());
    }
    let found: Vec<Arc<dyn DataSource>> = ctx.registry().names().iter().filter_map(|n| ctx.registry().lookup::<Arc<dyn DataSource>>(n).map(|d| (*d).clone())).collect();
    match found.len() {
        1 => Ok(found.into_iter().next().unwrap()),
        0 => Err(CamelError::illegal_argument("DataSource must be configured (dataSource=#name)")),
        _ => Err(CamelError::illegal_argument("Several DataSources are bound in the registry; choose one with dataSource=#name")),
    }
}

/// The `sql` component.
#[derive(Default)]
pub struct SqlComponent {
    default_datasource: Option<Arc<dyn DataSource>>,
}

impl SqlComponent {
    pub fn new() -> Self {
        Self::default()
    }
    /// A component whose endpoints use this data source unless `dataSource` is set.
    pub fn with_datasource(ds: Arc<dyn DataSource>) -> Self {
        SqlComponent { default_datasource: Some(ds) }
    }
}

/// Endpoint configuration.
#[derive(Clone)]
pub struct SqlConfig {
    pub query: String,
    pub datasource: Arc<dyn DataSource>,
    pub output_type: OutputType,
    pub output_header: Option<String>,
    pub use_iterator: bool,
    pub noop: bool,
    pub batch: bool,
    pub on_consume: Option<String>,
    pub on_consume_failed: Option<String>,
    pub on_consume_batch_complete: Option<String>,
    pub max_messages_per_poll: usize,
    pub route_empty_result_set: bool,
    pub use_message_body_for_sql: bool,
    pub parameters_count: usize,
    pub placeholder: String,
    pub separator: char,
    pub allow_named_parameters: bool,
    pub transacted: bool,
    pub break_batch_on_consume_fail: bool,
    pub expected_update_count: i64,
    pub max_rows: usize,
    pub scheduler: ScheduledPollConfig,
}

impl Component for SqlComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let query = resolve_query_text(remaining)?;
        let use_message_body_for_sql = params.take_bool("useMessageBodyForSql", false)?;
        if query.trim().is_empty() && !use_message_body_for_sql {
            return Err(CamelError::illegal_argument("Query must be specified (or use useMessageBodyForSql=true)"));
        }
        let ds_opt = params.take("dataSource").or_else(|| params.take("dataSourceRef"));
        let datasource = resolve_endpoint_datasource(ctx, ds_opt, self.default_datasource.as_ref())?;
        let output_type = match params.take("outputType") {
            Some(s) => OutputType::parse(&s)?,
            None => OutputType::SelectList,
        };
        let use_placeholder = params.take_bool("usePlaceholder", true)?;
        let placeholder = params.take_string("placeholder", "#");
        let separator = params.take_string("separator", ",");
        let separator = separator.chars().next().unwrap_or(',');
        let mut template = params.take_prefixed("template.");
        let max_rows = template.remove("maxRows").map(|v| v.parse::<i64>().unwrap_or(0).max(0) as usize).unwrap_or(0);
        for k in ["outputClass", "processingStrategy", "prepareStatementStrategy", "lazyStartProducer", "alwaysPopulateStatement", "rowMapperFactory"] {
            params.take(k);
        }
        let cfg = SqlConfig {
            query,
            datasource,
            output_type,
            output_header: params.take("outputHeader").filter(|s| !s.is_empty()),
            use_iterator: params.take_bool("useIterator", true)?,
            noop: params.take_bool("noop", false)?,
            batch: params.take_bool("batch", false)?,
            on_consume: params.take("onConsume").filter(|s| !s.trim().is_empty()),
            on_consume_failed: params.take("onConsumeFailed").filter(|s| !s.trim().is_empty()),
            on_consume_batch_complete: params.take("onConsumeBatchComplete").filter(|s| !s.trim().is_empty()),
            max_messages_per_poll: params.take_i64("maxMessagesPerPoll", 0)?.max(0) as usize,
            route_empty_result_set: params.take_bool("routeEmptyResultSet", false)?,
            use_message_body_for_sql,
            parameters_count: params.take_i64("parametersCount", 0)?.max(0) as usize,
            placeholder: if use_placeholder { placeholder } else { String::new() },
            separator,
            allow_named_parameters: params.take_bool("allowNamedParameters", true)?,
            transacted: params.take_bool("transacted", false)?,
            break_batch_on_consume_fail: params.take_bool("breakBatchOnConsumeFail", false)?,
            expected_update_count: params.take_i64("expectedUpdateCount", -1)?,
            max_rows,
            scheduler: ScheduledPollConfig::from_params(params, Duration::from_millis(1000), Duration::from_millis(500))?,
        };
        Ok(Arc::new(SqlEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg: Arc::new(cfg) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `sql:` endpoint.
pub struct SqlEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: Arc<SqlConfig>,
}

impl SqlEndpoint {
    pub fn config(&self) -> &SqlConfig {
        &self.cfg
    }
}

impl Endpoint for SqlEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(SqlProducer { cfg: self.cfg.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let core = Arc::new(SqlConsumerCore { cfg: self.cfg.clone(), ctx: self.ctx.clone(), logger: self.uri.clone() });
        let poll = ScheduledPollConsumer::new(self.uri.clone(), self.cfg.scheduler.clone(), move || core.poll(processor.as_ref()));
        Ok(Arc::new(SqlConsumer { poll }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn parse(cfg: &SqlConfig, sql: &str) -> Result<ParsedSql> {
    parse_sql(sql, &cfg.placeholder, ":#", cfg.allow_named_parameters)
}

/// Convert a SELECT result to a body according to the output type.
pub fn select_body(r: &QueryResult, output: OutputType) -> Result<Option<Value>> {
    match output {
        OutputType::SelectList | OutputType::StreamList => Ok(Some(Value::Array(r.rows_as_values()))),
        OutputType::SelectOne => match r.rows.len() {
            0 => Ok(None),
            1 => {
                if r.columns.len() == 1 {
                    Ok(Some(r.rows[0][0].clone()))
                } else {
                    Ok(r.rows_as_values().into_iter().next())
                }
            }
            n => Err(sql_error("21000", format!("Query result not unique for outputType=SelectOne. Got {} count instead.", n))),
        },
    }
}

// ---------------------------------------------------------------------------
// producer
// ---------------------------------------------------------------------------

struct SqlProducer {
    cfg: Arc<SqlConfig>,
}

impl Service for SqlProducer {}

impl SqlProducer {
    fn bind_for(&self, parsed: &ParsedSql, ex: &Exchange, item: Option<&Value>) -> Result<(String, Vec<Value>)> {
        let cfg = &self.cfg;
        let source = item.unwrap_or(ex.body());
        let body_map = source.as_object();
        let positional = if parsed.positional_count() == 0 {
            Vec::new()
        } else if cfg.use_message_body_for_sql && item.is_none() {
            ex.header(headers::SQL_PARAMETERS).map(|v| positional_values(v, cfg.separator)).unwrap_or_default()
        } else {
            positional_values(source, cfg.separator)
        };
        bind(parsed, ex, body_map, positional, cfg.parameters_count)
    }
}

impl Processor for SqlProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let cfg = &self.cfg;
        let sql_text = match ex.header(headers::SQL_QUERY).filter(|v| !v.is_null()) {
            Some(q) => q.to_string(),
            None if cfg.use_message_body_for_sql => {
                let s = ex.body_as::<String>()?;
                if s.trim().is_empty() {
                    return Err(CamelError::illegal_argument("Message body must contain the SQL query when useMessageBodyForSql=true"));
                }
                s
            }
            None => cfg.query.clone(),
        };
        let parsed = parse(cfg, &sql_text)?;
        let want_keys = ex.header(headers::SQL_RETRIEVE_GENERATED_KEYS).map(|v| v.is_truthy()).unwrap_or(false);
        let opts = ExecuteOptions { return_generated_keys: want_keys, max_rows: cfg.max_rows };
        if cfg.batch {
            let items = match ex.body() {
                Value::Array(a) => a.clone(),
                Value::Null => Vec::new(),
                other => vec![other.clone()],
            };
            let mut statements = Vec::with_capacity(items.len());
            for item in &items {
                statements.push(self.bind_for(&parsed, ex, Some(item))?);
            }
            let ds = cfg.datasource.clone();
            let total = crate::transaction::run_in_transaction(&ds, || {
                with_connection(&ds, |c| {
                    let mut total = 0i64;
                    for (sql, vals) in &statements {
                        total += c.execute_with(sql, vals, &opts)?.update_count.max(0);
                    }
                    Ok(total)
                })
            })?;
            ex.set_header(headers::SQL_UPDATE_COUNT, total);
            return Ok(());
        }
        let (sql, values) = self.bind_for(&parsed, ex, None)?;
        cameleer_core::camel_debug!("sql", "Executing SQL: {} with {} parameter(s)", sql, values.len());
        let r = with_connection(&cfg.datasource, |c| c.execute_with(&sql, &values, &opts))?;
        if r.is_query {
            ex.set_header(headers::SQL_ROW_COUNT, r.rows.len() as i64);
            if cfg.noop {
                return Ok(());
            }
            if let Some(body) = select_body(&r, cfg.output_type)? {
                match &cfg.output_header {
                    Some(h) => ex.set_header(h.clone(), body),
                    None => ex.set_body(body),
                }
            }
        } else {
            ex.set_header(headers::SQL_UPDATE_COUNT, r.update_count);
            if want_keys {
                let rows: Vec<Value> = r.generated_keys.iter().cloned().map(Value::Object).collect();
                ex.set_header(headers::SQL_GENERATED_KEYS_ROW_COUNT, rows.len() as i64);
                ex.set_header(headers::SQL_GENERATED_KEYS_DATA, Value::Array(rows));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// consumer
// ---------------------------------------------------------------------------

struct SqlConsumerCore {
    cfg: Arc<SqlConfig>,
    ctx: WeakContext,
    logger: String,
}

impl SqlConsumerCore {
    fn run_statement(&self, sql: &str, ex: &Exchange, row: Option<&Value>) -> Result<i64> {
        let parsed = parse(&self.cfg, sql)?;
        let positional = match row {
            Some(r) if parsed.positional_count() > 0 => positional_values(r, self.cfg.separator),
            _ => Vec::new(),
        };
        let (sql, vals) = bind(&parsed, ex, row.and_then(|r| r.as_object()), positional, 0)?;
        let r = with_connection(&self.cfg.datasource, |c| c.execute(&sql, &vals))?;
        Ok(r.update_count)
    }

    fn on_consume(&self, ex: &Exchange, rows: &[Value]) -> Result<()> {
        let Some(sql) = &self.cfg.on_consume else { return Ok(()) };
        for row in rows {
            let n = self.run_statement(sql, ex, Some(row))?;
            if self.cfg.expected_update_count >= 0 && n != self.cfg.expected_update_count {
                return Err(sql_error("02000", format!("Expected update count {} but was {} executing query: {}", self.cfg.expected_update_count, n, sql)));
            }
        }
        Ok(())
    }

    fn poll(&self, processor: &dyn Processor) -> Result<usize> {
        let cfg = &self.cfg;
        let tx = if cfg.transacted { BoundTransaction::begin(&cfg.datasource)? } else { None };
        let result = self.poll_inner(processor);
        match (tx, &result) {
            (Some(tx), Ok((_, false))) => tx.commit()?,
            (Some(tx), _) => tx.rollback()?,
            (None, _) => {}
        }
        result.map(|(n, _)| n)
    }

    /// Returns (exchanges processed, failed).
    fn poll_inner(&self, processor: &dyn Processor) -> Result<(usize, bool)> {
        let cfg = &self.cfg;
        let probe = Exchange::new(self.ctx.upgrade());
        let parsed = parse(cfg, &cfg.query)?;
        let (sql, vals) = bind(&parsed, &probe, None, Vec::new(), 0)?;
        let opts = ExecuteOptions { max_rows: cfg.max_rows, ..Default::default() };
        let r = with_connection(&cfg.datasource, |c| c.execute_with(&sql, &vals, &opts))?;
        if !r.is_query {
            return Err(CamelError::new(kinds::SQL_EXCEPTION, format!("Consumer query must be a SELECT: {}", cfg.query)));
        }
        let mut rows = r.rows_as_values();
        if cfg.max_messages_per_poll > 0 && rows.len() > cfg.max_messages_per_poll {
            rows.truncate(cfg.max_messages_per_poll);
        }
        if rows.is_empty() && !cfg.route_empty_result_set {
            return Ok((0, false));
        }
        let batches: Vec<(Value, Vec<Value>)> = if cfg.use_iterator && !rows.is_empty() {
            rows.into_iter().map(|r| (r.clone(), vec![r])).collect()
        } else {
            vec![(Value::Array(rows.clone()), rows)]
        };
        let total = batches.len();
        let mut failed = false;
        let mut processed = 0;
        for (i, (body, rows)) in batches.into_iter().enumerate() {
            let mut ex = Exchange::new(self.ctx.upgrade());
            ex.set_body(body);
            ex.set_property(props::BATCH_INDEX, i as i64);
            ex.set_property(props::BATCH_SIZE, total as i64);
            ex.set_property(props::BATCH_COMPLETE, i + 1 == total);
            if !cfg.use_iterator {
                ex.set_header(headers::SQL_ROW_COUNT, rows.len() as i64);
            }
            processed += 1;
            let ok = process_consumer_exchange(&self.logger, processor, &mut ex) && !ex.rollback_only;
            if ok {
                if let Err(e) = self.on_consume(&ex, &rows) {
                    cameleer_core::camel_warn!(&self.logger, "Error executing onConsume/onConsumeFailed query {}: {}", cfg.on_consume.as_deref().unwrap_or(""), e);
                    if cfg.break_batch_on_consume_fail || cfg.transacted {
                        failed = true;
                        break;
                    }
                }
            } else {
                if let Some(sql) = &cfg.on_consume_failed {
                    for row in &rows {
                        if let Err(e) = self.run_statement(sql, &ex, Some(row)) {
                            cameleer_core::camel_warn!(&self.logger, "Error executing onConsumeFailed query {}: {}", sql, e);
                        }
                    }
                }
                if cfg.transacted {
                    failed = true;
                    break;
                }
            }
        }
        if !failed {
            if let Some(sql) = &cfg.on_consume_batch_complete {
                let mut ex = Exchange::new(self.ctx.upgrade());
                ex.set_header("CamelBatchSize", total as i64);
                if let Err(e) = self.run_statement(sql, &ex, None) {
                    cameleer_core::camel_warn!(&self.logger, "Error executing onConsumeBatchComplete query {}: {}", sql, e);
                }
            }
        }
        Ok((processed, failed))
    }
}

/// The polling `sql` consumer.
pub struct SqlConsumer {
    poll: ScheduledPollConsumer,
}

impl SqlConsumer {
    /// Run one poll now; returns the number of exchanges created.
    pub fn poll_once(&self) -> Result<usize> {
        self.poll.poll_once()
    }
}

impl Service for SqlConsumer {
    fn start(&self) -> Result<()> {
        self.poll.start()
    }
    fn stop(&self) -> Result<()> {
        self.poll.stop()
    }
}

/// Map `values` into a parameter map (helper for tests and processors).
pub fn param_map<I: IntoIterator<Item = (&'static str, Value)>>(values: I) -> Value {
    Value::Object(values.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<ObjectMap>())
}
