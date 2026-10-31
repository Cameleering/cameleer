//! Cassandra: CQL native protocol v4 client, the Camel `cql` component and an embedded
//! test server with an in-memory CQL engine.
//!
//! `cql://host1,host2:9042/keyspace?cql=SELECT * FROM users WHERE id = ?&consistencyLevel=QUORUM`
//! or `cql:bean:sessionBean?cql=...` (a [`CqlSession`] bean).
//!
//! - **Producer**: runs `cql` (header `CamelCqlQuery` overrides it) with the body as bind
//!   values (an array, or a single value; null = no values) using prepared statements
//!   (`prepareStatements=false` sends plain QUERY frames with inferred value types). The
//!   result goes to the body according to `resultSetConversionStrategy`: `ALL` (default,
//!   array of row objects), `ONE` (first row or null) or `LIMIT_n` (first n rows).
//! - **Consumer**: polls `cql` (scheduled poll options `delay`, `initialDelay`, ...) and
//!   routes one exchange per poll with the converted result set (like Camel).
//!
//! Options: `cql`, `consistencyLevel` (default `LOCAL_ONE`), `serialConsistencyLevel`,
//! `prepareStatements` (true), `resultSetConversionStrategy`, `username`/`password`
//! (PasswordAuthenticator), `fetchSize` (page size, 5000), `connectTimeout`, `readTimeout`,
//! TLS with `ssl=true` / `sslContextParameters=#bean` / `useGlobalSslContextParameters`;
//! accepted and ignored: `clusterName`, `loadBalancingPolicyClass`, `datacenter`,
//! `extraTypeCodecs`, `session`.
//! Values: timestamps are returned as ISO-8601 text (numbers = epoch millis accepted on
//! input), uuids as text, dates as `YYYY-MM-DD`, decimals as text, blobs as bytes.

pub mod client;
pub mod engine;
pub mod frame;
pub mod server;

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use camel_core::component::scheduled::{ScheduledPollConfig, ScheduledPollConsumer};
use camel_core::component::support::process_consumer_exchange;
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

pub use client::{CqlConfig, CqlConnection, CqlResult, CqlSession, ExecOptions, Prepared, Rows};
pub use frame::{Consistency, CqlType};

use crate::util::{header_str, split_host_port};

/// Header names (Camel `CassandraConstants`).
pub mod headers {
    pub const CQL_QUERY: &str = "CamelCqlQuery";
}

/// How a result set becomes the body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultSetConversion {
    All,
    One,
    Limit(usize),
}

impl ResultSetConversion {
    pub fn parse(s: &str) -> Result<ResultSetConversion> {
        let u = s.to_ascii_uppercase();
        Ok(match u.as_str() {
            "ALL" => ResultSetConversion::All,
            "ONE" => ResultSetConversion::One,
            _ => match u.strip_prefix("LIMIT_").and_then(|n| n.parse().ok()) {
                Some(n) => ResultSetConversion::Limit(n),
                None => return Err(CamelError::illegal_argument(format!("Invalid resultSetConversionStrategy: {}", s))),
            },
        })
    }

    fn limit(self) -> Option<usize> {
        match self {
            ResultSetConversion::All => None,
            ResultSetConversion::One => Some(1),
            ResultSetConversion::Limit(n) => Some(n),
        }
    }

    /// Converts rows to the body.
    pub fn body(self, rows: &Rows) -> Value {
        let objs = rows.to_objects();
        match self {
            ResultSetConversion::All => Value::Array(objs),
            ResultSetConversion::One => objs.into_iter().next().unwrap_or(Value::Null),
            ResultSetConversion::Limit(n) => Value::Array(objs.into_iter().take(n).collect()),
        }
    }
}

/// The `cql` component.
#[derive(Default)]
pub struct CqlComponent;

/// Endpoint configuration.
#[derive(Clone)]
pub struct CqlEndpointConfig {
    pub session: Arc<CqlSession>,
    pub cql: Option<String>,
    pub options: ExecOptions,
    pub conversion: ResultSetConversion,
}

const IGNORED: &[&str] = &["clusterName", "loadBalancingPolicyClass", "loadBalancingPolicy", "datacenter", "extraTypeCodecs", "session", "lazyStartProducer", "bridgeErrorHandler", "exceptionHandler", "exchangePattern", "pollStrategy"];

impl Component for CqlComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let remaining = remaining.trim_start_matches('/');
        let consistency = match params.take("consistencyLevel") {
            Some(c) => Consistency::parse(&c)?,
            None => Consistency::LocalOne,
        };
        let serial = match params.take("serialConsistencyLevel") {
            Some(c) => Some(Consistency::parse(&c)?),
            None => None,
        };
        let conversion = match params.take("resultSetConversionStrategy") {
            Some(s) => ResultSetConversion::parse(&s)?,
            None => ResultSetConversion::All,
        };
        let options = ExecOptions { consistency, serial_consistency: serial, fetch_size: params.take_u64("fetchSize", 5000)?.clamp(1, i32::MAX as u64) as i32, limit: None, prepare: params.take_bool("prepareStatements", true)? };
        let session = if let Some(bean) = remaining.strip_prefix("bean:") {
            let name = bean.split('/').next().unwrap_or(bean);
            for k in ["username", "password", "connectTimeout", "readTimeout", "ssl", "sslContextParameters", "useGlobalSslContextParameters"] {
                params.take(k);
            }
            ctx.registry().lookup::<CqlSession>(name).ok_or_else(|| CamelError::no_such_bean(name))?
        } else {
            let (hostpart, keyspace) = match remaining.split_once('/') {
                Some((h, k)) => (h, Some(k.to_string()).filter(|k| !k.is_empty())),
                None => (remaining, None),
            };
            let hosts_list: Vec<&str> = hostpart.split(',').filter(|h| !h.is_empty()).collect();
            // a port given on the last host applies to all hosts (Camel syntax host1,host2:port)
            let shared_port = hosts_list.last().and_then(|h| split_host_port(h, 9042).ok()).map(|(_, p)| p).unwrap_or(9042);
            let mut hosts = Vec::new();
            for h in &hosts_list {
                let (host, port) = split_host_port(h, shared_port)?;
                hosts.push((host, port));
            }
            if hosts.is_empty() {
                hosts.push(("localhost".to_string(), 9042));
            }
            let tls = camel_tls::camel_support::endpoint_tls(ctx, params, "cql", false, &["ssl"])?;
            let cfg = CqlConfig {
                hosts,
                keyspace,
                username: params.take("username").filter(|u| !u.is_empty()),
                password: params.take("password"),
                tls,
                connect_timeout: params.take_duration("connectTimeout", Duration::from_secs(10))?,
                read_timeout: Some(params.take_duration("readTimeout", Duration::from_secs(30))?),
            };
            Arc::new(CqlSession::new(cfg))
        };
        let cfg = CqlEndpointConfig { session, cql: params.take("cql").filter(|c| !c.trim().is_empty()), options, conversion };
        let poll = ScheduledPollConfig::from_params(params, Duration::from_millis(1000), Duration::from_millis(500))?;
        for k in IGNORED {
            params.take(k);
        }
        Ok(Arc::new(CqlEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg, poll }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `cql` endpoint.
pub struct CqlEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: CqlEndpointConfig,
    poll: ScheduledPollConfig,
}

impl CqlEndpoint {
    pub fn config(&self) -> &CqlEndpointConfig {
        &self.cfg
    }
}

impl Endpoint for CqlEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(CqlProducer { cfg: self.cfg.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let cql = self.cfg.cql.clone().ok_or_else(|| CamelError::illegal_argument("A cql consumer needs the cql option"))?;
        let (cfg, ctx, logger) = (self.cfg.clone(), self.ctx.clone(), self.uri.clone());
        let consumer = ScheduledPollConsumer::new(format!("cql-consumer-{}", self.uri), self.poll.clone(), move || {
            let mut o = cfg.options.clone();
            o.limit = cfg.conversion.limit();
            let rows = cfg.session.execute(&cql, &[], &o)?;
            let mut ex = Exchange::new(ctx.upgrade());
            ex.set_body(cfg.conversion.body(&rows));
            process_consumer_exchange(&logger, processor.as_ref(), &mut ex);
            Ok(1)
        });
        Ok(Arc::new(CqlConsumer { inner: consumer, session: self.cfg.session.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct CqlConsumer {
    inner: ScheduledPollConsumer,
    session: Arc<CqlSession>,
}

impl Service for CqlConsumer {
    fn start(&self) -> Result<()> {
        self.inner.start()
    }
    fn stop(&self) -> Result<()> {
        let r = self.inner.stop();
        self.session.close();
        r
    }
}

struct CqlProducer {
    cfg: CqlEndpointConfig,
}

impl Service for CqlProducer {
    fn stop(&self) -> Result<()> {
        self.cfg.session.close();
        Ok(())
    }
}

impl Processor for CqlProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let cql = header_str(ex, headers::CQL_QUERY).or_else(|| self.cfg.cql.clone()).ok_or_else(|| CamelError::illegal_argument("cql: no query (cql option or CamelCqlQuery header)"))?;
        let values: Vec<Value> = match ex.body() {
            Value::Null => Vec::new(),
            Value::Array(a) => a.clone(),
            other => vec![other.clone()],
        };
        let mut o = self.cfg.options.clone();
        o.limit = self.cfg.conversion.limit();
        let rows = self.cfg.session.execute(&cql, &values, &o)?;
        ex.set_body(self.cfg.conversion.body(&rows));
        Ok(())
    }
}
