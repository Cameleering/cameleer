//! Management HTTP endpoints: health checks, Prometheus metrics and developer consoles
//! (like Camel Main's embedded HTTP server with `camel.server.*` options).
//!
//! | option | default | effect |
//! |---|---|---|
//! | `camel.server.enabled` | `false` | start the management server |
//! | `camel.server.host` | `0.0.0.0` | bind address |
//! | `camel.server.port` | `8080` | port (shared with `platform-http` routes on the same port) |
//! | `camel.server.devConsoleEnabled` | `false` | `/q/dev/*` |
//! | `camel.server.healthCheckEnabled` | `false` | `/q/health`, `/q/health/ready`, `/q/health/live` |
//! | `camel.server.metricsEnabled` | `false` | `/q/metrics` (Prometheus text) |
//!
//! On `camel.server.port` the endpoints speak what the port speaks: with
//! `camel.server.sslEnabled=true` they are HTTPS, next to the HTTPS routes. Camel's separate
//! management server is supported too: `camel.management.enabled=true` serves them on
//! `camel.management.port` (default `9876`) and `camel.management.host`, plain HTTP, with the
//! `camel.management.devConsoleEnabled`, `healthCheckEnabled` and `metricsEnabled` switches (the
//! `camel.server.*` switches count as well).
//!
//! Endpoints:
//!
//! - `GET /q/health`, `/q/health/ready`, `/q/health/live`: JSON, `200` when UP, `503` when DOWN
//! - `GET /q/metrics`: Prometheus text exposition
//! - `GET /q/dev`: list of consoles; `/q/dev/context`, `/q/dev/routes`, `/q/dev/route/{id}`,
//!   `/q/dev/endpoints`, `/q/dev/health`, `/q/dev/trace` (backlog tracer),
//!   `/q/dev/route-controller`, `/q/dev/templates`, `/q/dev/tools` (AI tools, needs the `ai`
//!   feature)
//! - `POST /q/dev/route/{id}/start|stop|suspend|resume|reset-stats`
//! - `POST /q/dev/tools/{name}/disable|enable` (the AI tools' kill switch)

use std::collections::BTreeMap;
use std::sync::Mutex;

use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::value::Value;
use cameleer_http::http::{HttpRequest, HttpResponse};
use cameleer_http::registry::{registry, HttpRouteSpec, Registration, RouteHandler};
use cameleer_http::server::ServerConfig;

/// Configuration of the management server.
#[derive(Clone, Debug, PartialEq)]
pub struct ConsoleConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub dev_console: bool,
    pub health: bool,
    pub metrics: bool,
    /// On the separate management port (`camel.management.*`) rather than `camel.server.port`.
    pub management_port: bool,
}

impl Default for ConsoleConfig {
    fn default() -> Self {
        ConsoleConfig { enabled: false, host: "0.0.0.0".into(), port: 8080, dev_console: false, health: false, metrics: false, management_port: false }
    }
}

impl ConsoleConfig {
    /// Everything enabled on `port` (as `cameleer run --console --port N`).
    pub fn all(port: u16) -> Self {
        ConsoleConfig { enabled: true, host: "0.0.0.0".into(), port, dev_console: true, health: true, metrics: true, management_port: false }
    }

    /// Read the `camel.server.*` options (dashed or camelCase keys).
    pub fn from_properties(props: &BTreeMap<String, String>) -> Result<Self> {
        let get = |k: &str| prop(props, k);
        let flag = |k: &str| get(k).map(|v| v.trim().eq_ignore_ascii_case("true")).unwrap_or(false);
        let mut c = ConsoleConfig {
            enabled: flag("camel.server.enabled"),
            dev_console: flag("camel.server.dev-console-enabled"),
            health: flag("camel.server.health-check-enabled"),
            metrics: flag("camel.server.metrics-enabled"),
            ..Default::default()
        };
        if let Some(h) = get("camel.server.host") {
            c.host = h;
        }
        if let Some(p) = get("camel.server.port") {
            c.port = p.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid camel.server.port: {}", p)))?;
        }
        // Camel's separate management server
        if flag("camel.management.enabled") {
            c.enabled = true;
            c.management_port = true;
            c.dev_console |= flag("camel.management.dev-console-enabled");
            c.health |= flag("camel.management.health-check-enabled");
            c.metrics |= flag("camel.management.metrics-enabled");
            c.host = get("camel.management.host").unwrap_or(c.host);
            c.port = match get("camel.management.port") {
                Some(p) => p.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid camel.management.port: {}", p)))?,
                None => 9876,
            };
        }
        Ok(c)
    }

    /// Whether the server should be started.
    pub fn is_active(&self) -> bool {
        self.enabled && (self.dev_console || self.health || self.metrics)
    }
}

/// Look up a property by its dashed name or camelCase alias.
pub(crate) fn prop(props: &BTreeMap<String, String>, dashed: &str) -> Option<String> {
    let mut camel = String::new();
    let mut upper = false;
    for c in dashed.chars() {
        if c == '-' {
            upper = true;
        } else if upper {
            camel.extend(c.to_uppercase());
            upper = false;
        } else {
            camel.push(c);
        }
    }
    props.get(dashed).or_else(|| props.get(&camel)).cloned()
}

/// A running management server (an `/q/*` registration on the shared HTTP server of the port).
pub struct DevConsoleServer {
    registration: Mutex<Option<Registration>>,
    port: u16,
}

impl DevConsoleServer {
    /// Start serving the management endpoints of `ctx`. Port 0 picks a free port.
    pub fn start(ctx: &CamelContext, config: &ConsoleConfig) -> Result<DevConsoleServer> {
        let weak: WeakContext = ctx.downgrade();
        let cfg = config.clone();
        let handler: RouteHandler = std::sync::Arc::new(move |req: &mut HttpRequest, _params: Vec<(String, String)>| match weak.upgrade() {
            Some(ctx) => handle_request(&ctx, &cfg, req),
            None => HttpResponse::text(503, "CamelContext is gone"),
        });
        let spec = HttpRouteSpec { path: "/q/*".into(), ..Default::default() };
        // on the shared port the endpoints speak what the port speaks (HTTPS with
        // camel.server.sslEnabled); the default pool size, because the first registration on a
        // port decides it for every route there
        let tls = if config.management_port { None } else { cameleer_http::platform::embedded_server_tls(ctx)? };
        let server_config = ServerConfig { name: "camel-console".into(), tls, ..Default::default() };
        let reg = registry().register(&config.host, config.port, &server_config, spec, handler)?;
        let port = reg.port;
        let scheme = if server_config.tls.is_some() { "https" } else { "http" };
        cameleer_core::camel_info!("Main", "Management HTTP server listening on {}://{}:{}/q (dev console: {}, health: {}, metrics: {})", scheme, config.host, port, config.dev_console, config.health, config.metrics);
        Ok(DevConsoleServer { registration: Mutex::new(Some(reg)), port })
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Stop serving (idempotent). The underlying server is stopped when no other
    /// consumer uses its port.
    pub fn stop(&self) {
        if let Some(r) = self.registration.lock().unwrap().take() {
            registry().unregister(&r);
        }
    }
}

impl Drop for DevConsoleServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn json(status: u16, v: &Value) -> HttpResponse {
    HttpResponse::json(status, v.to_json())
}

fn error(status: u16, msg: impl Into<String>) -> HttpResponse {
    json(status, &Value::object([("error", Value::from(msg.into()))]))
}

fn health_response(report: cameleer_core::health::HealthReport) -> HttpResponse {
    json(if report.is_up() { 200 } else { 503 }, &report.to_value())
}

const CONSOLES: &[(&str, &str, &str)] = &[
    ("context", "CamelContext", "Overall information about the CamelContext"),
    ("routes", "Routes", "Route information and statistics"),
    ("route", "Route", "One route: /q/dev/route/{id}"),
    ("endpoints", "Endpoints", "Endpoints used by the routes"),
    ("health", "Health Check", "Health check status"),
    ("trace", "Trace", "Backlog tracer events"),
    ("route-controller", "Route Controller", "Route controller status (supervising)"),
    ("templates", "Route Templates", "Registered route templates"),
    ("tools", "AI Tools", "AI tools (kill switch: POST /q/dev/tools/{name}/disable|enable) and pending approvals"),
];

/// Answer one management request (also usable without a server, e.g. in tests).
pub fn handle_request(ctx: &CamelContext, cfg: &ConsoleConfig, req: &HttpRequest) -> HttpResponse {
    let path = req.path.trim_end_matches('/');
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let method = req.method.as_str();
    let get = method == "GET" || method == "HEAD";
    match segs.as_slice() {
        ["q", "health"] if cfg.health && get => health_response(ctx.health()),
        ["q", "health", "ready"] if cfg.health && get => health_response(ctx.health_readiness()),
        ["q", "health", "live"] if cfg.health && get => health_response(ctx.health_liveness()),
        ["q", "metrics"] if cfg.metrics && get => {
            HttpResponse::text(200, ctx.managed_context().prometheus()).with_content_type("text/plain; version=0.0.4; charset=utf-8")
        }
        ["q", "dev", rest @ ..] if cfg.dev_console => dev_console(ctx, method, rest),
        _ => error(404, format!("No management endpoint for {} {}", method, req.path)),
    }
}

fn dev_console(ctx: &CamelContext, method: &str, rest: &[&str]) -> HttpResponse {
    let mc = ctx.managed_context();
    let get = method == "GET" || method == "HEAD";
    match rest {
        [] if get => {
            let list = CONSOLES
                .iter()
                .map(|(id, name, desc)| {
                    Value::object([
                        ("id", Value::from(*id)),
                        ("displayName", Value::from(*name)),
                        ("description", Value::from(*desc)),
                        ("path", Value::from(format!("/q/dev/{}", id))),
                    ])
                })
                .collect();
            json(200, &Value::object([("consoles", Value::Array(list))]))
        }
        ["context"] if get => json(200, &mc.to_value()),
        ["routes"] if get => json(200, &Value::object([("routes", mc.routes_value())])),
        ["route", id] if get => match mc.route(id) {
            Some(r) => json(200, &r.to_value()),
            None => error(404, format!("No route with id {}", id)),
        },
        ["route", id, action] if method == "POST" => {
            if mc.route(id).is_none() {
                return error(404, format!("No route with id {}", id));
            }
            let r = match *action {
                "start" => mc.start_route(id),
                "stop" => mc.stop_route(id),
                "suspend" => mc.suspend_route(id),
                "resume" => mc.resume_route(id),
                "reset-stats" => mc.reset_route_statistics(id),
                other => return error(400, format!("Unknown route action {}", other)),
            };
            match r {
                Ok(()) => json(
                    200,
                    &Value::object([
                        ("routeId", Value::from(*id)),
                        ("action", Value::from(*action)),
                        ("state", Value::from(ctx.route_status(id).map(|s| s.to_string()).unwrap_or_default())),
                    ]),
                ),
                Err(e) => error(500, e.to_string()),
            }
        }
        ["endpoints"] if get => json(200, &Value::object([("endpoints", mc.endpoints_value())])),
        ["health"] if get => json(200, &ctx.health().to_value()),
        ["trace"] if get => {
            let t = ctx.tracer();
            json(
                200,
                &Value::object([
                    ("enabled", Value::Bool(t.is_enabled())),
                    ("backlogEnabled", Value::Bool(t.is_backlog_enabled())),
                    ("events", t.backlog_value()),
                ]),
            )
        }
        ["route-controller"] if get => json(200, &ctx.route_controller_status().to_value()),
        ["templates"] if get => {
            let list = ctx
                .route_template_ids()
                .into_iter()
                .filter_map(|id| ctx.route_template(&id))
                .map(|t| {
                    let params = t
                        .parameters
                        .iter()
                        .map(|p| {
                            Value::object([
                                ("name", Value::from(p.name.as_str())),
                                ("required", Value::Bool(p.required)),
                                ("defaultValue", p.default.as_deref().map(Value::from).unwrap_or(Value::Null)),
                                ("description", p.description.as_deref().map(Value::from).unwrap_or(Value::Null)),
                            ])
                        })
                        .collect();
                    Value::object([("id", Value::from(t.id.as_str())), ("parameters", Value::Array(params))])
                })
                .collect();
            json(200, &Value::object([("templates", Value::Array(list))]))
        }
        ["tools"] if get => match crate::tools::tools_value(ctx) {
            Ok(v) => json(200, &v),
            Err(e) => error(if crate::tools::available() { 500 } else { 501 }, e.to_string()),
        },
        ["tools", name, action] if method == "POST" => {
            let enabled = match *action {
                "enable" => true,
                "disable" => false,
                other => return error(400, format!("Unknown tool action {}", other)),
            };
            match crate::tools::set_enabled(ctx, name, enabled) {
                Ok(()) => json(200, &Value::object([("tool", Value::from(*name)), ("action", Value::from(*action)), ("disabled", Value::Bool(!enabled))])),
                Err(e) if !crate::tools::available() => error(501, e.to_string()),
                Err(e) if e.to_string().contains("No tool named") => error(404, e.to_string()),
                Err(e) => error(500, e.to_string()),
            }
        }
        _ if !get && method != "POST" => error(405, "Method Not Allowed"),
        _ => error(404, format!("Unknown dev console: /q/dev/{}", rest.join("/"))),
    }
}

/// Camel-like startup summary:
///
/// ```text
/// Routes startup (total:2 started:2)
///     Started route1 (timer://foo)
///     Started route2 (direct://bar)
/// ```
pub fn startup_summary(ctx: &CamelContext) -> Vec<String> {
    let routes = ctx.routes();
    let started = routes.iter().filter(|r| r.status() == cameleer_core::ServiceStatus::Started).count();
    let mut head = format!("Routes startup (total:{} started:{}", routes.len(), started);
    if started < routes.len() {
        head.push_str(&format!(" stopped:{}", routes.len() - started));
    }
    head.push(')');
    let mut lines = vec![head];
    for r in &routes {
        let state = if r.status() == cameleer_core::ServiceStatus::Started { "Started" } else { "Stopped" };
        lines.push(format!("    {} {} ({})", state, r.id, r.endpoint_uri));
    }
    lines
}

fn cell(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(v) => v.to_string(),
    }
}

/// Format the JSON of `/q/dev/routes` as a table (`cameleer get routes`).
pub fn format_routes_table(v: &Value) -> String {
    let headers = ["ID", "FROM", "STATUS", "AGE", "TOTAL", "FAIL", "INFLIGHT", "MEAN", "MIN", "MAX", "LAST"];
    let mut rows: Vec<Vec<String>> = Vec::new();
    let empty = Vec::new();
    let routes = v.get("routes").and_then(|r| r.as_array()).unwrap_or(&empty);
    for r in routes {
        let s = r.get("statistics");
        let st = |k: &str| cell(s.and_then(|s| s.get(k)));
        rows.push(vec![
            cell(r.get("routeId")),
            cell(r.get("from")),
            cell(r.get("state")),
            cell(r.get("uptime")),
            st("exchangesTotal"),
            st("exchangesFailed"),
            cell(r.get("exchangesInflight")),
            st("meanProcessingTime"),
            st("minProcessingTime"),
            st("maxProcessingTime"),
            st("lastProcessingTime"),
        ]);
    }
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in &rows {
        for (i, c) in row.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let fmt_row = |cells: Vec<String>| -> String {
        let parts: Vec<String> = cells.iter().enumerate().map(|(i, c)| format!("{:<w$}", c, w = widths[i])).collect();
        parts.join("  ").trim_end().to_string()
    };
    let mut out = fmt_row(headers.iter().map(|s| s.to_string()).collect());
    out.push('\n');
    for row in rows {
        out.push_str(&fmt_row(row));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_properties() {
        let mut p = BTreeMap::new();
        p.insert("camel.server.enabled".to_string(), "true".to_string());
        p.insert("camel.server.devConsoleEnabled".to_string(), "true".to_string());
        p.insert("camel.server.port".to_string(), "9876".to_string());
        let c = ConsoleConfig::from_properties(&p).unwrap();
        assert!(c.enabled && c.dev_console && !c.health && !c.metrics);
        assert_eq!(c.port, 9876);
        assert!(c.is_active());
        p.insert("camel.server.port".to_string(), "x".to_string());
        assert!(ConsoleConfig::from_properties(&p).is_err());
        assert!(!ConsoleConfig::from_properties(&BTreeMap::new()).unwrap().is_active());
    }

    #[test]
    fn routes_table() {
        let v = cameleer_core::json::parse(
            r#"{"routes":[{"routeId":"r1","from":"direct:a","state":"Started","uptime":"1s","exchangesInflight":0,
            "statistics":{"exchangesTotal":3,"exchangesFailed":1,"meanProcessingTime":2,"minProcessingTime":1,"maxProcessingTime":4,"lastProcessingTime":1}}]}"#,
        )
        .unwrap();
        let t = format_routes_table(&v);
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[0].starts_with("ID  FROM      STATUS   AGE  TOTAL  FAIL"), "{}", t);
        assert!(lines[1].starts_with("r1  direct:a  Started  1s   3      1"), "{}", t);
    }
}
