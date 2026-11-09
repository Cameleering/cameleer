//! DICOM for Cameleer (std only).
//!
//! - [`vr`], [`tag`], [`dict`]: Value Representations, tags and the PS3.6 data dictionary.
//! - [`charset`]: Specific Character Set decoding and encoding.
//! - [`object`], [`codec`], [`ts`]: data sets, Part 10 files and transfer syntaxes.
//! - [`model`]: data sets as Value trees (the DICOM JSON model) and back, byte-exact.
//! - [`dataformat`]: the `dicom` and `dicomJson` data formats.
//! - [`path`]: tag paths for `${dicom.PatientID}` in Simple and the `dicom` language.
//! - [`ul`], [`assoc`], [`dimse`], [`scu`], [`scp`]: DIMSE networking (PS3.7, PS3.8).
//! - [`component`]: the `dicom:` component (SCP consumer, SCU producer).
//!
//! [`register`] adds all of it to a context.

use std::collections::BTreeMap;
use std::sync::{Arc, Once};

use cameleer_core::context::CamelContext;
use cameleer_core::dataformat::{register_data_format_factory, DataFormat};
use cameleer_core::error::Result;
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Expression, Language};
use cameleer_core::value::Value;

pub mod assoc;
pub mod charset;
pub mod codec;
pub mod component;
pub mod dataformat;
pub mod dict;
pub mod dimse;
pub mod headers;
pub mod model;
pub mod object;
pub mod path;
pub mod scp;
pub mod scu;
pub mod tag;
pub mod testing;
pub mod ts;
pub mod ul;
pub mod vr;

pub use codec::{DicomFile, ReadOptions};
pub use object::{DataSet, Element, Item};

/// The `dicom` language: a tag path evaluated on the message body (`PatientID`,
/// `(0010,0020)`, `ReferencedSeriesSequence[0].SeriesInstanceUID`).
pub struct DicomLanguage;

impl Language for DicomLanguage {
    fn name(&self) -> &str {
        "dicom"
    }
    fn create_expression(&self, text: &str, _options: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        let p = path::TagPath::parse(text)?;
        Ok(Arc::new(move |ex: &Exchange| -> Result<Value> { path::evaluate_on_exchange(ex, &p) }))
    }
}

fn register_simple() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        cameleer_core::language::simple::register_function(
            "dicom",
            Arc::new(|ex: &Exchange, rest: &str| {
                let p = path::TagPath::parse(rest)?;
                path::evaluate_on_exchange(ex, &p)
            }),
        );
    });
}

/// Register the `dicom` component, the `dicom` and `dicomJson` data formats, the `dicom`
/// language and the `${dicom.<path>}` Simple function.
pub fn register(ctx: &CamelContext) {
    register_simple();
    ctx.add_component_factory("dicom", || Arc::new(component::DicomComponent));
    let reg = ctx.registry();
    register_data_format_factory(reg, "dicom", |o| Ok(Arc::new(dataformat::DicomDataFormat::from_options(o)?) as Arc<dyn DataFormat>));
    register_data_format_factory(reg, "dicomJson", |o| Ok(Arc::new(dataformat::DicomJsonDataFormat::from_options(o)?) as Arc<dyn DataFormat>));
    if ctx.resolve_language("dicom").is_err() {
        ctx.add_language(DicomLanguage);
    }
}
