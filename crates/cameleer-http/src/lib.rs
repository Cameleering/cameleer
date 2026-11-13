//! # Cameleer HTTP
//!
//! HTTP/1.1 server and client built on `std::net`, with HTTPS (client and server) via
//! `cameleer-tls` and `SslContextParameters`, plus the Camel components built on top of them:
//!
//! | scheme | role |
//! |---|---|
//! | `platform-http`, `jetty`, `undertow`, `netty-http` | HTTP consumers ([`platform`]) |
//! | `http`, `https` | HTTP producer ([`producer`]) |
//! | `rest` | rest endpoints ([`rest::RestComponent`]) |
//! | `netty` | TCP/UDP sockets ([`netty`]) |
//!
//! and the REST DSL ([`rest::RestDslExt`]).
//!
//! ```no_run
//! use cameleer_core::prelude::*;
//! use cameleer_http::rest::RestDslExt;
//!
//! let ctx = CamelContext::new();
//! cameleer_http::register(&ctx);
//! ctx.properties().set("camel.server.port", "8080");
//! ctx.add_routes(|b| {
//!     b.from("platform-http:/hello?httpMethodRestrict=GET").set_body(simple("Hello ${header.name}"));
//!     b.from("timer:t?period=5000").to("http://localhost:8080/hello?name=timer").log("${body}");
//!     b.rest("/users").get("/{id}").to("direct:user");
//!     b.from("direct:user").set_body(simple("user ${header.id}"));
//!     b.from("netty:tcp://0.0.0.0:5150?textline=true&sync=true").transform(simple("echo: ${body}"));
//! }).unwrap();
//! ctx.start().unwrap();
//! ```

pub mod binding;
pub mod client;
pub mod http;
pub mod netty;
pub mod platform;
pub mod producer;
pub mod registry;
pub mod rest;
pub mod server;
pub mod server_simoom;

use std::sync::Arc;

use cameleer_core::context::CamelContext;

pub use binding::headers;
pub use client::{HttpClient, HttpConnection, ProxyAddr, ProxyConfig, Url};
pub use http::{HttpHeaders, HttpRequest, HttpResponse};
pub use producer::HttpOperationFailed;
pub use rest::{RestBindingMode, RestDslExt, RestParam};
pub use server::{HttpServer, ServerConfig};

/// Register all components of this crate on a context (lazily created):
/// `platform-http`, `jetty`, `undertow`, `netty-http`, `http`, `https`,
/// `netty` and `rest`. Schemes that already have a component (or factory) registered
/// are left untouched, so applications can pre-register customized components.
pub fn register(ctx: &CamelContext) {
    for scheme in ["platform-http", "jetty", "undertow", "netty-http"] {
        if !ctx.has_component(scheme) {
            ctx.add_component_factory(scheme, move || Arc::new(platform::HttpConsumerComponent::new(scheme)));
        }
    }
    for scheme in ["http", "https"] {
        if !ctx.has_component(scheme) {
            ctx.add_component_factory(scheme, move || Arc::new(producer::HttpComponent::new(scheme)));
        }
    }
    if !ctx.has_component("netty") {
        ctx.add_component_factory("netty", || Arc::new(netty::NettyComponent));
    }
    if !ctx.has_component("rest") {
        ctx.add_component_factory("rest", || Arc::new(rest::RestComponent));
    }
}

/// The actual port of a started HTTP consumer (`platform-http:`, `jetty:`...) or `netty:`
/// consumer endpoint — useful with port 0. For `rest:` endpoints use the generated
/// `platform-http` uri or [`platform_http_port`].
pub fn local_port(ctx: &CamelContext, uri: &str) -> Option<u16> {
    let ep = ctx.get_endpoint(uri).ok()?;
    if let Some(e) = ep.as_any().downcast_ref::<platform::HttpConsumerEndpoint>() {
        return e.local_port();
    }
    if let Some(e) = ep.as_any().downcast_ref::<netty::NettyEndpoint>() {
        return e.local_port();
    }
    None
}

/// The ephemeral port shared by `platform-http` endpoints configured with port 0
/// (e.g. `camel.server.port=0`, or `rest_configuration().port(0)`).
pub fn platform_http_port(ctx: &CamelContext) -> Option<u16> {
    let c = ctx.get_component("platform-http")?;
    c.as_any().downcast_ref::<platform::HttpConsumerComponent>()?.shared_port()
}
