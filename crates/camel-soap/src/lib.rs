//! # camel-soap
//!
//! A std-only SOAP / web services stack for rust-camel plus JAXB-style binding and FHIR:
//!
//! | module | contents |
//! |---|---|
//! | [`wsdl`] | WSDL 1.1 parser (types, messages, portTypes, SOAP 1.1 / 1.2 bindings, document and rpc literal, services, ports, `soapAction`) |
//! | [`envelope`] | SOAP 1.1 / 1.2 envelopes, faults (`faultcode`/`faultstring`/`detail`, `Code`/`Subcode`/`Reason`/`Detail`) |
//! | [`wssec`] | WS-Security UsernameToken (text / digest), timestamps, envelope signatures (camel-xml `xmlsecurity`) |
//! | [`xsdmodel`], [`pojo`] | XSD-typed XML ⇄ `Value` binding; POJO-mode parameter mapping |
//! | [`cxf`] | the `cxf:` component (producer + consumer, `POJO` / `PAYLOAD` / `RAW` / `CXF_MESSAGE`) |
//! | [`cxfrs`] | the `cxfrs:` component (JAX-RS style REST consumer / producer) |
//! | [`springws`] | the `spring-ws:` component (consumer by root QName / SOAP action / URI / XPath result, producer) |
//! | [`jaxb`] | the `jaxb` data format (schema-typed, structural fallback) |
//! | [`fhir`] | FHIR R4: `fhirJson` / `fhirXml` data formats, JSON ⇄ XML conversion, the `fhir:` component, mock server |
//!
//! ```no_run
//! use camel_core::prelude::*;
//! let ctx = CamelContext::new();
//! camel_http::register(&ctx);
//! camel_soap::register(&ctx);
//! ctx.add_routes(|b| {
//!     b.from("cxf://http://0.0.0.0:9000/calculator?wsdlURL=file:calc.wsdl")
//!         .set_body(simple("${body[0]}"));
//!     b.from("direct:add").to("cxf://http://localhost:9000/calculator?wsdlURL=file:calc.wsdl&defaultOperationName=Add");
//! }).unwrap();
//! ```

pub mod cxf;
pub mod cxfrs;
pub mod envelope;
pub mod fhir;
pub mod jaxb;
pub mod pojo;
pub mod springws;
pub mod support;
pub mod testing;
pub mod transport;
pub mod wsdl;
pub mod wssec;
pub mod xsdmodel;

use std::sync::Arc;

use camel_core::context::CamelContext;
use camel_core::dataformat::register_data_format_factory;

pub use envelope::{SoapFault, SoapVersion};
pub use wsdl::Wsdl;

pub(crate) fn register_kinds() {
    envelope::register_kinds();
    jaxb::register_kinds();
    springws::register_kinds();
    fhir::register_kinds();
}

/// Register the components `cxf`, `cxfrs`, `spring-ws`, `fhir` and the data formats
/// `jaxb`, `fhirJson`, `fhirXml` on a context.
pub fn register(ctx: &CamelContext) {
    register_kinds();
    ctx.add_component_factory("cxf", || Arc::new(cxf::CxfComponent));
    ctx.add_component_factory("cxfrs", || Arc::new(cxfrs::CxfRsComponent));
    ctx.add_component_factory("spring-ws", || Arc::new(springws::SpringWsComponent));
    ctx.add_component_factory("fhir", || Arc::new(fhir::component::FhirComponent));
    let reg = ctx.registry();
    register_data_format_factory(reg, "jaxb", jaxb::factory);
    register_data_format_factory(reg, "fhirJson", fhir::dataformat::json_factory);
    register_data_format_factory(reg, "fhirXml", fhir::dataformat::xml_factory);
}

/// The actual port of a started `cxf:`, `cxfrs:` or `spring-ws:` consumer (useful with port 0).
pub fn local_port(ctx: &CamelContext, uri: &str) -> Option<u16> {
    let ep = ctx.get_endpoint(uri).ok()?;
    if let Some(e) = ep.as_any().downcast_ref::<cxf::CxfEndpoint>() {
        return e.local_port();
    }
    if let Some(e) = ep.as_any().downcast_ref::<cxfrs::CxfRsEndpoint>() {
        return e.local_port();
    }
    if let Some(e) = ep.as_any().downcast_ref::<springws::SpringWsEndpoint>() {
        return e.local_port();
    }
    None
}
