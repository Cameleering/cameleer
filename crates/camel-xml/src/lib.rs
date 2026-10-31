//! # camel-xml
//!
//! A std-only XML stack for rust-camel:
//!
//! | module | contents |
//! |---|---|
//! | [`parser`], [`dom`], [`serialize`] | XML 1.0 + namespaces parser (DTD internal subset entities, CDATA, PIs, comments, UTF-8 / ISO-8859-1 / UTF-16), arena DOM, XML/HTML/text serializer |
//! | [`xpath`], [`language`] | complete XPath 1.0 and the Camel `xpath` language (replaces camel-core's subset) |
//! | [`xquery`], [`xquery_component`] | XQuery 1.0 processor, the `xquery` language and the `xquery:` component |
//! | [`xslt`], [`xslt_component`] | XSLT 1.0 processor and the `xslt` / `xslt-saxon` components |
//! | [`xsd`], [`validator`] | XML Schema subset and the `validator` component (replaces the well-formedness check) |
//! | [`xj`] | the `xj` component (XML ⇄ JSON) |
//! | [`c14n`], [`xmlsecurity`] | exclusive XML canonicalization, `xmlsecurity-sign` / `xmlsecurity-verify` (enveloped XML Signature, RSA-SHA256 / ECDSA-SHA256) |
//!
//! ```no_run
//! use camel_core::prelude::*;
//! let ctx = CamelContext::new();
//! camel_xml::register(&ctx);
//! ctx.add_routes(|b| {
//!     b.from("direct:in").to("xslt:file:templates/order.xsl").to("validator:file:schemas/invoice.xsd");
//! }).unwrap();
//! ```
//!
//! XQuery: [`xquery`] (XQuery 1.0 + common 3.x conveniences) and [`xquery_component`]
//! (`xquery` language, `xquery:` component).

pub mod c14n;
pub mod dom;
pub mod language;
pub mod parser;
pub mod serialize;
pub mod validator;
pub mod xj;
pub mod xmlsecurity;
pub mod xpath;
pub mod xquery;
pub mod xsd;
pub mod xslt;
pub mod xslt_component;
pub mod xquery_component;

use std::sync::{Arc, OnceLock};

use camel_core::context::CamelContext;
use camel_core::error::{kinds, register_kind_parent};

pub use dom::{Document, NodeId, NodeType, QName};
pub use language::xpath_ns;
pub use xpath::{XPath, XValue};
pub use xquery::XQuery;
pub use xquery_component::xquery_expr;

/// `SAXParseException`: XML is not well-formed.
pub const SAX_PARSE_EXCEPTION: &str = "SAXParseException";
/// `SAXException`, parent of [`SAX_PARSE_EXCEPTION`].
pub const SAX_EXCEPTION: &str = "SAXException";
/// `XPathExpressionException`: XPath evaluation failure.
pub const XPATH_EXPRESSION_EXCEPTION: &str = "XPathExpressionException";
/// `TransformerException`: XSLT compilation or execution failure.
pub const TRANSFORMER_EXCEPTION: &str = "TransformerException";
/// `TransformerConfigurationException` (a `TransformerException`): invalid stylesheet.
pub const TRANSFORMER_CONFIGURATION_EXCEPTION: &str = "TransformerConfigurationException";

pub(crate) fn register_kinds() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        register_kind_parent(SAX_EXCEPTION, kinds::EXCEPTION);
        register_kind_parent(SAX_PARSE_EXCEPTION, SAX_EXCEPTION);
        register_kind_parent(XPATH_EXPRESSION_EXCEPTION, kinds::EXCEPTION);
        register_kind_parent(TRANSFORMER_EXCEPTION, kinds::EXCEPTION);
        register_kind_parent(TRANSFORMER_CONFIGURATION_EXCEPTION, TRANSFORMER_EXCEPTION);
        validator::register_validation_kinds();
        xmlsecurity::register_kinds();
        xquery_component::register_kinds();
    });
}

/// Register the XML stack on a context: the `xpath` language (replacing camel-core's
/// subset), the `xquery` language and the `xquery`, `xslt`, `xslt-saxon`, `validator` (replacing the well-formedness
/// check), `xj`, `xmlsecurity-sign` and `xmlsecurity-verify` components.
pub fn register(ctx: &CamelContext) {
    register_kinds();
    ctx.add_language(language::XPathLanguage);
    ctx.add_language(xquery_component::XQueryLanguage);
    ctx.add_component_factory("xquery", || Arc::new(xquery_component::XQueryComponent));
    ctx.add_component_factory("xslt", || Arc::new(xslt_component::XsltComponent::new(false)));
    ctx.add_component_factory("xslt-saxon", || Arc::new(xslt_component::XsltComponent::new(true)));
    ctx.add_component_factory("validator", || Arc::new(validator::ValidatorComponent));
    ctx.add_component_factory("xj", || Arc::new(xj::XjComponent));
    ctx.add_component_factory("xmlsecurity-sign", || Arc::new(xmlsecurity::XmlSecurityComponent::sign()));
    ctx.add_component_factory("xmlsecurity-verify", || Arc::new(xmlsecurity::XmlSecurityComponent::verify()));
}
