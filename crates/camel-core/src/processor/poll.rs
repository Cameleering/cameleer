//! Poll EIP (Camel 4 `poll`): receive a single message from an endpoint with a polling
//! consumer; the received message replaces the current message (a null body when nothing
//! arrives within the timeout). The uri may be dynamic (Simple, `${...}`).

use std::sync::Arc;

use crate::builder::{constant, simple};
use crate::error::Result;
use crate::impl_definition_boilerplate;
use crate::model::{ProcessorDefinition, RouteContext};
use crate::processor::routing::PollEnrichDefinition;
use crate::spi::Processor;

/// Default poll timeout (20 seconds, as in Camel).
pub const DEFAULT_POLL_TIMEOUT_MS: i64 = 20_000;

/// Model of the `poll` EIP.
pub struct PollDefinition {
    pub id: Option<String>,
    pub uri: String,
    /// Timeout in millis: -1 waits forever, 0 does not wait.
    pub timeout_ms: i64,
}

impl PollDefinition {
    pub fn new(uri: impl Into<String>, timeout_ms: i64) -> Self {
        PollDefinition { id: None, uri: uri.into(), timeout_ms }
    }
}

impl ProcessorDefinition for PollDefinition {
    fn name(&self) -> &str {
        "poll"
    }
    fn label(&self) -> String {
        format!("poll[{}]", self.uri)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let uri = if self.uri.contains("${") { simple(self.uri.clone()).def } else { constant(self.uri.clone()).def };
        PollEnrichDefinition::new(uri, self.timeout_ms).create_processor(rc)
    }
    impl_definition_boilerplate!();
}
