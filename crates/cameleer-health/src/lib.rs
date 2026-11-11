//! The Cameleer health pack: healthcare integration in one optional crate.
//!
//! | crate feature | adds |
//! |---|---|
//! | `hl7` | the `mllp` component ([`hl7::mllp`]), the `hl7` data format and the `hl7terser` language ([`hl7`]) |
//! | `fhir` | the `fhir` component and the `fhirJson` and `fhirXml` data formats ([`fhir`]) |
//! | `dicom` | the `dicom` component (DIMSE SCU and SCP) and the `dicomweb` component, the `dicom`, `dicomJson` and `dicomDeidentify` data formats, the `dicom` language and `${dicom.*}` in Simple ([`dicom`]) |
//!
//! All three are on by default. Add the pack to a context with
//! `ctx.add_pack(&cameleer_health::HEALTH)`, or build `cameleer-main` with `--features health`.
//! The pack is not part of Cameleer's `full` build: see `docs/packs/index.md`.
//!
//! ```no_run
//! use cameleer_core::prelude::*;
//!
//! let ctx = CamelContext::new();
//! ctx.add_pack(&cameleer_health::HEALTH);
//! ctx.add_routes(|b| {
//!     b.from("mllp://0.0.0.0:2575").to("log:hl7");
//! })
//! .unwrap();
//! ```

#[cfg(feature = "dicom")]
pub mod dicom;
#[cfg(feature = "fhir")]
pub mod fhir;
#[cfg(feature = "hl7")]
pub mod hl7;

use cameleer_core::context::CamelContext;
use cameleer_core::pack::{Pack, PackInfo};

/// What the health pack adds (with the crate features of this build).
pub static PACK: PackInfo = PackInfo {
    name: "health",
    version: env!("CARGO_PKG_VERSION"),
    core_api: cameleer_core::VERSION,
    description: "Healthcare integration: HL7 v2 and MLLP, FHIR R4, DICOM (DIMSE and DICOMweb) and PS3.15 de-identification",
    components: &[
        #[cfg(feature = "hl7")]
        "mllp",
        #[cfg(feature = "fhir")]
        "fhir",
        #[cfg(feature = "dicom")]
        "dicom",
        #[cfg(feature = "dicom")]
        "dicomweb",
    ],
    data_formats: &[
        #[cfg(feature = "hl7")]
        "hl7",
        #[cfg(feature = "fhir")]
        "fhirJson",
        #[cfg(feature = "fhir")]
        "fhirXml",
        #[cfg(feature = "dicom")]
        "dicom",
        #[cfg(feature = "dicom")]
        "dicomJson",
        #[cfg(feature = "dicom")]
        "dicomDeidentify",
    ],
    languages: &[
        #[cfg(feature = "hl7")]
        "hl7terser",
        #[cfg(feature = "dicom")]
        "dicom",
    ],
    simple_prefixes: &[
        #[cfg(feature = "dicom")]
        "dicom",
    ],
};

/// The health pack.
pub struct Health;

/// The health pack, for [`CamelContext::add_pack`] and `Main::add_pack`.
pub static HEALTH: Health = Health;

impl Pack for Health {
    fn info(&self) -> &'static PackInfo {
        &PACK
    }

    fn register(&self, ctx: &CamelContext) {
        register_parts(ctx);
    }
}

/// Add the health pack to a context (the same as `ctx.add_pack(&HEALTH)`).
pub fn register(ctx: &CamelContext) {
    ctx.add_pack(&HEALTH);
}

fn register_parts(ctx: &CamelContext) {
    #[cfg(feature = "hl7")]
    hl7::register(ctx);
    #[cfg(feature = "fhir")]
    fhir::register(ctx);
    #[cfg(feature = "dicom")]
    dicom::register(ctx);
    let _ = ctx;
}

/// The bound port of a started `mllp:`, `dicom:` or `dicomweb:` consumer (useful with port 0).
pub fn local_port(ctx: &CamelContext, uri: &str) -> Option<u16> {
    let ep = ctx.get_endpoint(uri).ok()?;
    let any = ep.as_any();
    #[cfg(feature = "hl7")]
    if let Some(e) = any.downcast_ref::<hl7::mllp::MllpEndpoint>() {
        return e.local_port();
    }
    #[cfg(feature = "dicom")]
    {
        if let Some(e) = any.downcast_ref::<dicom::component::DicomEndpoint>() {
            return e.local_port();
        }
        if let Some(e) = any.downcast_ref::<dicom::dicomweb::DicomWebEndpoint>() {
            return e.local_port();
        }
    }
    let _ = any;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pack_describes_itself() {
        let ctx = CamelContext::new();
        ctx.add_pack(&HEALTH);
        assert_eq!(ctx.packs(), vec![&PACK]);
        for c in PACK.components {
            assert!(ctx.component_names().iter().any(|n| n == c), "component {c}");
        }
        for l in PACK.languages {
            assert!(ctx.resolve_language(l).is_ok(), "language {l}");
        }
        for f in PACK.data_formats {
            assert!(cameleer_core::dataformat::lookup_data_format(ctx.registry(), f).is_some() || cameleer_core::dataformat::lookup_data_format_factory(ctx.registry(), f).is_some(), "data format {f}");
        }
        assert!(cameleer_core::pack::is_pack_language("hl7terser") || !cfg!(feature = "hl7"));
    }
}
