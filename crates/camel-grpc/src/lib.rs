//! # rust-camel gRPC
//!
//! A std-only implementation of the protocol stack behind Camel's `camel-grpc` and
//! `camel-protobuf`:
//!
//! | module | |
//! |---|---|
//! | [`hpack`] | HPACK header compression (RFC 7541) |
//! | [`h2`] | HTTP/2 client and server (RFC 9113), h2c prior knowledge and TLS + ALPN `h2` (camel-tls) |
//! | [`proto`] | `.proto` parser, descriptors, dynamic protobuf codec, proto3 JSON mapping |
//! | [`grpc`] | gRPC framing, status codes, deadlines, metadata, byte-level client/server |
//! | [`component`] | the Camel `grpc` component |
//! | [`dataformat`] | the Camel `protobuf` data format |
//!
//! ```no_run
//! use camel_core::prelude::*;
//!
//! let ctx = CamelContext::new();
//! camel_grpc::register(&ctx);
//! ctx.add_routes(|b| {
//!     // implement helloworld.Greeter/SayHello
//!     b.from("grpc://0.0.0.0:50051/helloworld.Greeter?protoFile=proto/helloworld.proto")
//!         .set_body(simple("${body}"))
//!         .process(|ex: &mut Exchange| {
//!             let name = ex.body().get("name").map(|v| v.to_string()).unwrap_or_default();
//!             ex.set_body(Value::object([("message", format!("Hello {}", name))]));
//!             Ok(())
//!         });
//!     // call it
//!     b.from("timer:t?period=5000")
//!         .set_body(constant(Value::object([("name", "Camel")])))
//!         .to("grpc://localhost:50051/helloworld.Greeter?method=sayHello&protoFile=proto/helloworld.proto")
//!         .log("${body}");
//! }).unwrap();
//! ctx.start().unwrap();
//! ```

pub mod component;
pub mod dataformat;
pub mod grpc;
pub mod h2;
pub mod hpack;
pub mod proto;

use std::sync::Arc;

use camel_core::context::CamelContext;
use camel_core::dataformat::lookup_data_format;

pub use component::{GrpcComponent, GrpcEndpoint, GRPC_EVENT_TYPE, GRPC_METHOD_NAME, GRPC_USER_AGENT};
pub use dataformat::{protobuf, protobuf_json, ContentTypeFormat, ProtobufDataFormat};
pub use grpc::{Code, Status};
pub use proto::DescriptorPool;

/// Registers the `grpc` component (lazily) and binds the `protobuf` data format in the
/// registry (configured per exchange / via `camel.dataformat.protobuf.*` properties, see
/// [`dataformat`]). Existing registrations are left untouched.
pub fn register(ctx: &CamelContext) {
    camel_core::error::register_kind_parent(grpc::STATUS_RUNTIME_EXCEPTION, camel_core::error::kinds::RUNTIME);
    camel_core::error::register_kind_parent("InvalidProtocolBufferException", camel_core::error::kinds::IO);
    if !ctx.has_component("grpc") {
        ctx.add_component_factory("grpc", || Arc::new(GrpcComponent));
    }
    // A factory (not a bound instance) so that named references with options, e.g. YAML
    // `marshal: {protobuf: {instanceClass, protoFile}}`, get a configured data format.
    // Without options it falls back to the dynamic one (CamelProtobuf* headers / properties).
    if lookup_data_format(ctx.registry(), "protobuf").is_none() {
        camel_core::dataformat::register_data_format_factory(ctx.registry(), "protobuf", |o| {
            Ok(Arc::new(ProtobufDataFormat::from_options(o)?) as Arc<dyn camel_core::dataformat::DataFormat>)
        });
    }
}

/// The bound port of a started `grpc` consumer endpoint (useful with port 0).
pub fn local_port(ctx: &CamelContext, uri: &str) -> Option<u16> {
    let ep = ctx.get_endpoint(uri).ok()?;
    ep.as_any().downcast_ref::<GrpcEndpoint>()?.local_port()
}
