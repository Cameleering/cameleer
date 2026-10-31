//! Transformation EIPs: `marshal` and `unmarshal` with data formats.

use std::sync::Arc;

use crate::dataformat::{DataFormat, DataFormatDefinition};
use crate::error::Result;
use crate::impl_definition_boilerplate;
use crate::message::Exchange;
use crate::model::{ProcessorDefinition, RouteContext};
use crate::spi::Processor;

/// Model of the `marshal` / `unmarshal` EIPs.
pub struct MarshalDefinition {
    pub id: Option<String>,
    pub data_format: DataFormatDefinition,
    /// true = unmarshal, false = marshal.
    pub unmarshal: bool,
}

impl MarshalDefinition {
    pub fn marshal(df: DataFormatDefinition) -> Self {
        MarshalDefinition { id: None, data_format: df, unmarshal: false }
    }
    pub fn unmarshal(df: DataFormatDefinition) -> Self {
        MarshalDefinition { id: None, data_format: df, unmarshal: true }
    }
}

/// Runtime processor invoking a data format.
pub struct MarshalProcessor {
    pub data_format: Arc<dyn DataFormat>,
    pub unmarshal: bool,
}

impl Processor for MarshalProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        if self.unmarshal {
            self.data_format.unmarshal(exchange)
        } else {
            self.data_format.marshal(exchange)
        }
    }
}

impl ProcessorDefinition for MarshalDefinition {
    fn name(&self) -> &str {
        if self.unmarshal {
            "unmarshal"
        } else {
            "marshal"
        }
    }
    fn label(&self) -> String {
        format!("{}[{:?}]", self.name(), self.data_format)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let df = self.data_format.resolve(&rc.context)?;
        Ok(Arc::new(MarshalProcessor { data_format: df, unmarshal: self.unmarshal }))
    }
    impl_definition_boilerplate!();
}
