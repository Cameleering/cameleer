//! Transformation DSL methods: `marshal` / `unmarshal` with data formats.

use crate::dataformat::DataFormatDefinition;
use crate::model::RouteDefinition;
use crate::processor::transform::MarshalDefinition;

impl RouteDefinition {
    /// Marshal the body with a data format, e.g.
    /// `.marshal(DataFormatDefinition::json().pretty_print(true))` or `.marshal("csv")`.
    pub fn marshal(&mut self, df: impl Into<DataFormatDefinition>) -> &mut Self {
        self.add(Box::new(MarshalDefinition::marshal(df.into())))
    }

    /// Unmarshal the body with a data format, e.g. `.unmarshal(DataFormatDefinition::csv().delimiter(';'))`.
    pub fn unmarshal(&mut self, df: impl Into<DataFormatDefinition>) -> &mut Self {
        self.add(Box::new(MarshalDefinition::unmarshal(df.into())))
    }

    /// Marshal the body to JSON text.
    pub fn marshal_json(&mut self) -> &mut Self {
        self.marshal(DataFormatDefinition::json())
    }

    /// Unmarshal JSON text to a structured body.
    pub fn unmarshal_json(&mut self) -> &mut Self {
        self.unmarshal(DataFormatDefinition::json())
    }
}
