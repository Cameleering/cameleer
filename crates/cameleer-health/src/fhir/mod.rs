//! FHIR R4 support:
//!
//! * [`convert`] — FHIR JSON ⇄ XML conversion following the FHIR serialization rules, using
//!   the small embedded type registry of [`model`] and the lexeme-preserving JSON tree of
//!   [`json`];
//! * [`dataformat`] — the `fhirJson` and `fhirXml` data formats (resources as [`Value`]s);
//! * [`component`] — the `fhir:` component (REST client: create / read / update / delete /
//!   search / history / transaction / capabilities / load-page);
//! * [`testing`] — an embedded in-memory FHIR server ([`testing::FhirMock`]).
//!
//! [`Value`]: cameleer_core::value::Value

pub mod component;
pub mod convert;
pub mod dataformat;
pub mod json;
pub mod model;
pub mod testing;
pub mod validation;

use cameleer_core::error::{kinds, register_kind_parent};

pub use convert::{json_to_xml, xml_to_json};

/// `ca.uhn.fhir.parser.DataFormatException`: invalid FHIR content.
pub const DATA_FORMAT_EXCEPTION: &str = "DataFormatException";
/// Base of the FHIR client errors (`BaseServerResponseException`).
pub const BASE_SERVER_RESPONSE_EXCEPTION: &str = "BaseServerResponseException";
/// HTTP 400.
pub const INVALID_REQUEST_EXCEPTION: &str = "InvalidRequestException";
/// HTTP 401.
pub const AUTHENTICATION_EXCEPTION: &str = "AuthenticationException";
/// HTTP 403.
pub const FORBIDDEN_OPERATION_EXCEPTION: &str = "ForbiddenOperationException";
/// HTTP 404.
pub const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";
/// HTTP 405.
pub const METHOD_NOT_ALLOWED_EXCEPTION: &str = "MethodNotAllowedException";
/// HTTP 409.
pub const RESOURCE_VERSION_CONFLICT_EXCEPTION: &str = "ResourceVersionConflictException";
/// HTTP 410.
pub const RESOURCE_GONE_EXCEPTION: &str = "ResourceGoneException";
/// HTTP 412.
pub const PRECONDITION_FAILED_EXCEPTION: &str = "PreconditionFailedException";
/// HTTP 422.
pub const UNPROCESSABLE_ENTITY_EXCEPTION: &str = "UnprocessableEntityException";
/// HTTP 5xx.
pub const INTERNAL_ERROR_EXCEPTION: &str = "InternalErrorException";

pub fn register_kinds() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        register_kind_parent(DATA_FORMAT_EXCEPTION, kinds::RUNTIME);
        register_kind_parent(BASE_SERVER_RESPONSE_EXCEPTION, kinds::RUNTIME);
        for k in [
            INVALID_REQUEST_EXCEPTION,
            AUTHENTICATION_EXCEPTION,
            FORBIDDEN_OPERATION_EXCEPTION,
            RESOURCE_NOT_FOUND_EXCEPTION,
            METHOD_NOT_ALLOWED_EXCEPTION,
            RESOURCE_VERSION_CONFLICT_EXCEPTION,
            RESOURCE_GONE_EXCEPTION,
            PRECONDITION_FAILED_EXCEPTION,
            UNPROCESSABLE_ENTITY_EXCEPTION,
            INTERNAL_ERROR_EXCEPTION,
        ] {
            register_kind_parent(k, BASE_SERVER_RESPONSE_EXCEPTION);
        }
    });
}

/// An element serialized as a self-contained XML fragment (every namespace in scope declared).
pub(crate) fn xml_fragment(doc: &cameleer_xml::dom::Document, el: cameleer_xml::dom::NodeId) -> String {
    let mut d = cameleer_xml::dom::Document::new();
    d.import_node(doc, el, 0);
    d.to_xml_string()
}

/// The text without a leading byte order mark and XML declaration.
pub(crate) fn strip_xml_decl(t: &str) -> &str {
    let t = t.trim_start_matches('\u{feff}').trim_start();
    if t.starts_with("<?xml") {
        if let Some(i) = t.find("?>") {
            return t[i + 2..].trim_start();
        }
    }
    t
}

/// Register the `fhir` component and the `fhirJson` and `fhirXml` data formats.
pub fn register(ctx: &cameleer_core::context::CamelContext) {
    register_kinds();
    ctx.add_component_factory("fhir", || std::sync::Arc::new(component::FhirComponent));
    let reg = ctx.registry();
    cameleer_core::dataformat::register_data_format_factory(reg, "fhirJson", dataformat::json_factory);
    cameleer_core::dataformat::register_data_format_factory(reg, "fhirXml", dataformat::xml_factory);
}
