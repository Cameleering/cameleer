//! `graphql:` component (Camel `camel-graphql`): sends GraphQL queries and mutations over
//! HTTP(S).
//!
//! `graphql://http://localhost:8080/graphql?query={books{id name}}` (the remaining part is
//! the endpoint url; without a scheme `https://` is assumed).
//!
//! Options: `query` (inline), `queryFile` (a file with the query), `queryHeader` (header
//! holding the query), `operationName`, `variables` (a JSON object or `#bean` holding a map),
//! `variablesHeader`, `accessToken` + `jwtAuthorizationType` (default `Bearer`),
//! `username` / `password` (Basic), `throwExceptionOnFailure` (default true).
//! Without a query option the body is the query; with one, an object body supplies the
//! variables. The body becomes the response JSON text (as Camel returns it);
//! `CamelGraphqlResponseCode` holds the HTTP status.

use std::any::Any;
use std::sync::Arc;

use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;

use crate::rest::{body_text, header_str, http_failed, Req, Rest};

pub const GRAPHQL_RESPONSE_CODE: &str = "CamelGraphqlResponseCode";

/// The `graphql` component.
#[derive(Default)]
pub struct GraphqlComponent;

#[derive(Clone, Debug)]
struct Config {
    url: String,
    query: Option<String>,
    query_header: Option<String>,
    operation_name: Option<String>,
    variables: Option<String>,
    variables_header: Option<String>,
    access_token: Option<String>,
    jwt_type: String,
    username: Option<String>,
    password: Option<String>,
    throw_on_failure: bool,
}

impl Component for GraphqlComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let r = remaining.trim().trim_start_matches('/');
        if r.is_empty() {
            return Err(CamelError::illegal_argument("graphql: an endpoint url is required"));
        }
        let url = if r.starts_with("http://") || r.starts_with("https://") {
            r.to_string()
        } else if let Some(rest) = r.strip_prefix("http:/").or_else(|| r.strip_prefix("http:")) {
            format!("http://{}", rest.trim_start_matches('/'))
        } else if let Some(rest) = r.strip_prefix("https:/").or_else(|| r.strip_prefix("https:")) {
            format!("https://{}", rest.trim_start_matches('/'))
        } else {
            format!("https://{}", r)
        };
        let mut query = params.take("query").filter(|s| !s.is_empty());
        if let Some(f) = params.take("queryFile") {
            let p = cameleer_core::component::template_support::resource_path(&f);
            query = Some(std::fs::read_to_string(&p).map_err(|e| CamelError::new(cameleer_core::error::kinds::FILE_NOT_FOUND, format!("graphql: cannot read queryFile {}: {}", f, e)))?);
        }
        let variables = match params.take("variables") {
            Some(v) if v.starts_with('#') => {
                let name = v.trim_start_matches("#bean:").trim_start_matches('#');
                let bean = ctx.registry().lookup::<Value>(name).ok_or_else(|| CamelError::no_such_bean(name))?;
                Some((*bean).to_json())
            }
            other => other,
        };
        let cfg = Config {
            url,
            query,
            query_header: params.take("queryHeader"),
            operation_name: params.take("operationName"),
            variables,
            variables_header: params.take("variablesHeader"),
            access_token: params.take("accessToken").filter(|s| !s.is_empty()),
            jwt_type: params.take("jwtAuthorizationType").unwrap_or_else(|| "Bearer".into()),
            username: params.take("username"),
            password: params.take("password"),
            throw_on_failure: params.take_bool("throwExceptionOnFailure", true)?,
        };
        for k in ["proxyHost", "httpClient", "lazyStartProducer"] {
            params.take(k);
        }
        Ok(Arc::new(GraphqlEndpoint { uri: uri.to_string(), cfg, _ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A graphql endpoint (producer only).
pub struct GraphqlEndpoint {
    uri: String,
    cfg: Config,
    _ctx: WeakContext,
}

impl Endpoint for GraphqlEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(GraphqlProducer { cfg: self.cfg.clone(), rest: Rest::default() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct GraphqlProducer {
    cfg: Config,
    rest: Rest,
}

impl Service for GraphqlProducer {}

impl Processor for GraphqlProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let cfg = &self.cfg;
        let from_header = cfg.query_header.as_ref().and_then(|h| header_str(ex, h));
        let (query, body_vars) = match from_header.or_else(|| cfg.query.clone()) {
            Some(q) => (q, matches!(ex.message.body, Value::Object(_)).then(|| ex.message.body.clone())),
            None => (body_text(ex).ok_or_else(|| CamelError::illegal_argument("graphql: no query (query option, queryFile, queryHeader or body)"))?, None),
        };
        let mut req = vec![("query", Value::String(query))];
        if let Some(op) = &cfg.operation_name {
            req.push(("operationName", Value::String(op.clone())));
        }
        let vars = match cfg.variables_header.as_ref().and_then(|h| ex.message.headers.get(h).cloned()) {
            Some(Value::String(s)) => Some(cameleer_core::json::parse(&s)?),
            Some(v) if !v.is_null() => Some(v),
            _ => match &cfg.variables {
                Some(s) => Some(cameleer_core::json::parse(s)?),
                None => body_vars,
            },
        };
        if let Some(v) = vars {
            req.push(("variables", v));
        }
        let mut r = Req::post(cfg.url.clone()).json(&Value::object(req));
        if let Some(t) = &cfg.access_token {
            r = r.header("Authorization", format!("{} {}", cfg.jwt_type, t));
        } else if let (Some(u), Some(p)) = (&cfg.username, &cfg.password) {
            r = r.basic(u, p);
        }
        let resp = self.rest.send(r)?;
        ex.message.headers.set(GRAPHQL_RESPONSE_CODE, Value::Int(resp.status as i64));
        if cfg.throw_on_failure && !resp.is_success() {
            return Err(http_failed(&cfg.url, &resp));
        }
        ex.message.body = Value::String(resp.text());
        Ok(())
    }
}
