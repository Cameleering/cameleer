//! Service Call EIP (Camel `serviceCall`): call a remote service by name. The service
//! instances come from service discovery (static lists, custom [`ServiceDiscovery`]
//! implementations or registry references; several are combined), are filtered
//! (healthy, blacklist, custom [`ServiceFilter`]) and one is chosen by a load balancer
//! (round robin by default, or random).
//!
//! The endpoint uri is built from (in order of precedence):
//! * `expression`: evaluated with the `CamelServiceCallService*` headers set;
//! * `uri`: a template with `{host}`, `{port}`, `{name}` and `{path}` placeholders, or a
//!   uri containing the service name, which is replaced by `host:port`
//!   (`http://my-service/api` -> `http://10.0.0.1:8080/api`);
//! * otherwise `{component}://{host}:{port}/{path}` (component `http` by default).
//!
//! The name may carry a context path (`service_call("my-service/api/hello")`). The default
//! exchange pattern is `InOut`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use crate::builder::ExpressionDefinition;
use crate::error::{kinds, CamelError, Result};
use crate::impl_definition_boilerplate;
use crate::message::{Exchange, ExchangePattern};
use crate::model::{ProcessorDefinition, RouteContext};
use crate::spi::{Expression, Processor};

/// Header: the service name.
pub const SERVICE_CALL_SERVICE_NAME: &str = "CamelServiceCallServiceName";
/// Header: the chosen host.
pub const SERVICE_CALL_SERVICE_HOST: &str = "CamelServiceCallServiceHost";
/// Header: the chosen port.
pub const SERVICE_CALL_SERVICE_PORT: &str = "CamelServiceCallServicePort";
/// Header: the context path from the service name.
pub const SERVICE_CALL_CONTEXT_PATH: &str = "CamelServiceCallContextPath";
/// Header: the component (scheme).
pub const SERVICE_CALL_SCHEME: &str = "CamelServiceCallScheme";
/// Header: the configured uri.
pub const SERVICE_CALL_URI: &str = "CamelServiceCallUri";

/// One instance of a service.
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceDefinition {
    /// Service name (`None` = matches any name, for static servers given without `name@`).
    pub name: Option<String>,
    pub host: String,
    pub port: u16,
    pub healthy: bool,
    pub meta: BTreeMap<String, String>,
}

impl ServiceDefinition {
    pub fn new(name: Option<&str>, host: impl Into<String>, port: u16) -> Self {
        ServiceDefinition { name: name.map(String::from), host: host.into(), port, healthy: true, meta: BTreeMap::new() }
    }

    /// Parse `[name@]host[:port]` (port default 80).
    pub fn parse(s: &str) -> Result<ServiceDefinition> {
        let s = s.trim();
        let (name, addr) = match s.split_once('@') {
            Some((n, a)) => (Some(n.trim()), a.trim()),
            None => (None, s),
        };
        let (host, port) = match addr.rsplit_once(':') {
            Some((h, p)) => (h, p.parse::<u16>().map_err(|_| CamelError::illegal_argument(format!("Invalid service port in '{}'", s)))?),
            None => (addr, 80),
        };
        if host.is_empty() {
            return Err(CamelError::illegal_argument(format!("Invalid service address '{}'", s)));
        }
        Ok(ServiceDefinition::new(name, host, port))
    }

    /// Parse a comma separated list of servers.
    pub fn parse_list(s: &str) -> Result<Vec<ServiceDefinition>> {
        s.split(',').filter(|p| !p.trim().is_empty()).map(ServiceDefinition::parse).collect()
    }

    fn matches(&self, name: &str) -> bool {
        self.name.as_deref().map(|n| n == name).unwrap_or(true)
    }
}

/// Finds the instances of a service.
pub trait ServiceDiscovery: Send + Sync {
    fn services(&self, name: &str) -> Result<Vec<ServiceDefinition>>;
}

impl<F> ServiceDiscovery for F
where
    F: Fn(&str) -> Result<Vec<ServiceDefinition>> + Send + Sync,
{
    fn services(&self, name: &str) -> Result<Vec<ServiceDefinition>> {
        self(name)
    }
}

/// Filters the discovered instances.
pub trait ServiceFilter: Send + Sync {
    fn apply(&self, exchange: &Exchange, services: Vec<ServiceDefinition>) -> Vec<ServiceDefinition>;
}

impl<F> ServiceFilter for F
where
    F: Fn(&Exchange, Vec<ServiceDefinition>) -> Vec<ServiceDefinition> + Send + Sync,
{
    fn apply(&self, exchange: &Exchange, services: Vec<ServiceDefinition>) -> Vec<ServiceDefinition> {
        self(exchange, services)
    }
}

/// Service discovery configuration.
#[derive(Clone)]
pub enum DiscoveryConfig {
    /// A static list (`staticServiceDiscovery`).
    Static(Vec<ServiceDefinition>),
    /// A custom implementation.
    Custom(Arc<dyn ServiceDiscovery>),
    /// A `ServiceDiscovery` bound in the registry (as `Arc<dyn ServiceDiscovery>`).
    Ref(String),
}

/// Service filter configuration.
#[derive(Clone)]
pub enum FilterConfig {
    /// Keep healthy instances only (`healthyServiceFilter`).
    Healthy,
    /// Drop the listed instances (`blacklistServiceFilter`).
    Blacklist(Vec<ServiceDefinition>),
    /// Drop nothing.
    PassThrough,
    Custom(Arc<dyn ServiceFilter>),
}

/// Load balancer used to choose an instance.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum ServiceLoadBalancer {
    #[default]
    RoundRobin,
    Random,
}

/// Model of the service call EIP.
pub struct ServiceCallDefinition {
    pub id: Option<String>,
    pub name: String,
    pub uri: Option<String>,
    pub component: String,
    pub pattern: ExchangePattern,
    pub expression: Option<ExpressionDefinition>,
    pub discovery: Vec<DiscoveryConfig>,
    pub filters: Vec<FilterConfig>,
    pub load_balancer: ServiceLoadBalancer,
}

impl ServiceCallDefinition {
    pub fn new(name: impl Into<String>) -> Self {
        ServiceCallDefinition {
            id: None,
            name: name.into(),
            uri: None,
            component: "http".into(),
            pattern: ExchangePattern::InOut,
            expression: None,
            discovery: Vec::new(),
            filters: Vec::new(),
            load_balancer: ServiceLoadBalancer::RoundRobin,
        }
    }
    /// Add static servers (`[name@]host:port,...`).
    pub fn static_servers(mut self, servers: &str) -> Result<Self> {
        self.discovery.push(DiscoveryConfig::Static(ServiceDefinition::parse_list(servers)?));
        Ok(self)
    }
    pub fn uri(mut self, uri: impl Into<String>) -> Self {
        self.uri = Some(uri.into());
        self
    }
    pub fn component(mut self, c: impl Into<String>) -> Self {
        self.component = c.into();
        self
    }
    pub fn load_balancer(mut self, lb: ServiceLoadBalancer) -> Self {
        self.load_balancer = lb;
        self
    }
    pub fn filter(mut self, f: FilterConfig) -> Self {
        self.filters.push(f);
        self
    }
    pub fn discovery(mut self, d: DiscoveryConfig) -> Self {
        self.discovery.push(d);
        self
    }
    pub fn expression(mut self, e: impl Into<ExpressionDefinition>) -> Self {
        self.expression = Some(e.into());
        self
    }
}

struct ServiceCallProcessor {
    name: String,
    context_path: Option<String>,
    uri: Option<String>,
    component: String,
    pattern: ExchangePattern,
    expression: Option<Arc<dyn Expression>>,
    discovery: Vec<Arc<dyn ServiceDiscovery>>,
    filters: Vec<FilterConfig>,
    load_balancer: ServiceLoadBalancer,
    counter: AtomicUsize,
    seed: AtomicU64,
}

impl ServiceCallProcessor {
    fn choose(&self, n: usize) -> usize {
        match self.load_balancer {
            ServiceLoadBalancer::RoundRobin => self.counter.fetch_add(1, Ordering::Relaxed) % n,
            ServiceLoadBalancer::Random => {
                // xorshift64*
                let mut x = self.seed.load(Ordering::Relaxed);
                x ^= x >> 12;
                x ^= x << 25;
                x ^= x >> 27;
                self.seed.store(x, Ordering::Relaxed);
                (x.wrapping_mul(0x2545F4914F6CDD1D) % n as u64) as usize
            }
        }
    }

    fn build_uri(&self, s: &ServiceDefinition) -> String {
        let hp = format!("{}:{}", s.host, s.port);
        let path = self.context_path.clone().unwrap_or_default();
        match &self.uri {
            Some(u) if u.contains('{') => u
                .replace("{host}", &s.host)
                .replace("{port}", &s.port.to_string())
                .replace("{name}", &self.name)
                .replace("{path}", &path),
            Some(u) if u.contains(&self.name) => u.replacen(&self.name, &hp, 1),
            Some(u) => u.clone(),
            None => {
                let mut out = format!("{}://{}", self.component, hp);
                if !path.is_empty() {
                    out.push('/');
                    out.push_str(path.trim_start_matches('/'));
                }
                out
            }
        }
    }
}

impl Processor for ServiceCallProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let mut services = Vec::new();
        for d in &self.discovery {
            services.extend(d.services(&self.name)?.into_iter().filter(|s| s.matches(&self.name)));
        }
        for f in &self.filters {
            services = match f {
                FilterConfig::Healthy => services.into_iter().filter(|s| s.healthy).collect(),
                FilterConfig::Blacklist(b) => services
                    .into_iter()
                    .filter(|s| !b.iter().any(|x| x.host == s.host && x.port == s.port && x.matches(&self.name)))
                    .collect(),
                FilterConfig::PassThrough => services,
                FilterConfig::Custom(c) => c.apply(exchange, services),
            };
        }
        if services.is_empty() {
            return Err(CamelError::new(kinds::REJECTED_EXECUTION, format!("No active services with name {}", self.name)));
        }
        let s = &services[self.choose(services.len())];
        exchange.set_header(SERVICE_CALL_SERVICE_NAME, self.name.clone());
        exchange.set_header(SERVICE_CALL_SERVICE_HOST, s.host.clone());
        exchange.set_header(SERVICE_CALL_SERVICE_PORT, s.port as i64);
        exchange.set_header(SERVICE_CALL_SCHEME, self.component.clone());
        if let Some(p) = &self.context_path {
            exchange.set_header(SERVICE_CALL_CONTEXT_PATH, p.clone());
        }
        if let Some(u) = &self.uri {
            exchange.set_header(SERVICE_CALL_URI, u.clone());
        }
        let uri = match &self.expression {
            Some(e) => e.evaluate(exchange)?.to_string(),
            None => self.build_uri(s),
        };
        let ctx = exchange.context()?.clone();
        let producer = ctx.get_producer(uri.trim())?;
        let saved = exchange.pattern;
        exchange.pattern = self.pattern;
        exchange.set_property(crate::message::props::TO_ENDPOINT, ctx.normalize_endpoint_uri(uri.trim()));
        let r = producer.process(exchange);
        exchange.pattern = saved;
        r
    }
}

impl ProcessorDefinition for ServiceCallDefinition {
    fn name(&self) -> &str {
        "serviceCall"
    }
    fn label(&self) -> String {
        format!("serviceCall[{}]", self.name)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let ctx = &rc.context;
        let full = ctx.resolve_placeholders(&self.name)?;
        let (name, context_path) = match full.split_once('/') {
            Some((n, p)) => (n.to_string(), Some(p.to_string())),
            None => (full, None),
        };
        let mut discovery: Vec<Arc<dyn ServiceDiscovery>> = Vec::new();
        for d in &self.discovery {
            discovery.push(match d {
                DiscoveryConfig::Static(list) => {
                    let list = list.clone();
                    Arc::new(move |_n: &str| Ok(list.clone()))
                }
                DiscoveryConfig::Custom(c) => c.clone(),
                DiscoveryConfig::Ref(r) => {
                    let r = r.trim_start_matches('#').trim_start_matches("bean:");
                    ctx.registry()
                        .lookup::<Arc<dyn ServiceDiscovery>>(r)
                        .map(|a| (*a).clone())
                        .ok_or_else(|| CamelError::new(kinds::NO_SUCH_BEAN, format!("No ServiceDiscovery named {} in the registry", r)))?
                }
            });
        }
        if discovery.is_empty() {
            if let Some(d) = ctx.registry().lookup::<Arc<dyn ServiceDiscovery>>("serviceDiscovery") {
                discovery.push((*d).clone());
            } else {
                return Err(CamelError::illegal_argument(format!("No service discovery configured for service call {}", name)));
            }
        }
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1;
        Ok(Arc::new(ServiceCallProcessor {
            name,
            context_path,
            uri: match &self.uri {
                Some(u) => Some(ctx.resolve_placeholders(u)?),
                None => None,
            },
            component: self.component.clone(),
            pattern: self.pattern,
            expression: match &self.expression {
                Some(e) => Some(e.resolve_expression(ctx)?),
                None => None,
            },
            discovery,
            filters: self.filters.clone(),
            load_balancer: self.load_balancer,
            counter: AtomicUsize::new(0),
            seed: AtomicU64::new(seed),
        }))
    }
    impl_definition_boilerplate!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_servers() {
        let l = ServiceDefinition::parse_list("svc@a:1, b:2,svc@c").unwrap();
        assert_eq!(l[0], ServiceDefinition::new(Some("svc"), "a", 1));
        assert_eq!(l[1], ServiceDefinition::new(None, "b", 2));
        assert_eq!(l[2].port, 80);
        assert!(ServiceDefinition::parse("x@:1").is_err());
    }
}
