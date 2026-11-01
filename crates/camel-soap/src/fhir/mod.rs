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
//! [`Value`]: camel_core::value::Value

pub mod component;
pub mod convert;
pub mod dataformat;
pub mod json;
pub mod model;
pub mod testing;
pub mod validation;

use camel_core::error::{kinds, register_kind_parent};

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

pub(crate) fn register_kinds() {
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
