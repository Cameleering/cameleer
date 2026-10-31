//! `dataformat:` component - `dataformat:name:marshal|unmarshal?options`.
//!
//! Uses a data format bound in the registry under `name`, or creates a built-in one
//! (json, csv, base64, gzipDeflater, ...) with the endpoint options.

use std::any::Any;
use std::sync::Arc;

use crate::context::CamelContext;
use crate::dataformat::{create_data_format_with, lookup_data_format, DataFormat};
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Component, Endpoint, Processor, Producer, Service};
use crate::util::Parameters;

#[derive(Default)]
pub struct DataFormatComponent;

pub struct DataFormatEndpoint {
    uri: String,
    format: Arc<dyn DataFormat>,
    marshal: bool,
}

impl Component for DataFormatComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let (name, op) = remaining
            .rsplit_once(':')
            .ok_or_else(|| CamelError::illegal_argument("Expected dataformat:name:marshal|unmarshal"))?;
        let marshal = match op {
            "marshal" => true,
            "unmarshal" => false,
            other => return Err(CamelError::illegal_argument(format!("Operation must be marshal or unmarshal, was: {}", other))),
        };
        let format = match lookup_data_format(ctx.registry(), name) {
            Some(f) => {
                params.take_all();
                f
            }
            None => create_data_format_with(ctx.registry(), name, &params.take_all())?,
        };
        Ok(Arc::new(DataFormatEndpoint { uri: uri.to_string(), format, marshal }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct DataFormatProducer {
    format: Arc<dyn DataFormat>,
    marshal: bool,
}

impl Service for DataFormatProducer {}

impl Processor for DataFormatProducer {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        if self.marshal {
            self.format.marshal(exchange)
        } else {
            self.format.unmarshal(exchange)
        }
    }
}

impl Endpoint for DataFormatEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(DataFormatProducer { format: self.format.clone(), marshal: self.marshal }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    #[test]
    fn json_and_base64_via_endpoint() {
        let ctx = CamelContext::new();
        ctx.start().unwrap();
        let t = ctx.create_producer_template();
        let v = t.request_body("dataformat:json:unmarshal", r#"{"a":1}"#).unwrap();
        assert_eq!(v.get("a"), Some(&Value::from(1)));
        let enc = t.request_body("dataformat:base64:marshal", "hello").unwrap();
        let dec = t.request_body("dataformat:base64:unmarshal", enc).unwrap();
        assert_eq!(dec.to_string(), "hello");
        assert!(t.request_body("dataformat:json:frobnicate", "x").is_err());
    }
}
