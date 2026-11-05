//! `ref:` component - look up an endpoint bound in the registry.
//!
//! Bind the endpoint as `Arc<dyn Endpoint>`:
//! ```ignore
//! let ep: Arc<dyn Endpoint> = ctx.get_endpoint("seda:foo")?;
//! ctx.registry().bind("myEndpoint", ep);
//! // ... from("ref:myEndpoint") / to("ref:myEndpoint")
//! ```
//! A registry entry of type `String` is treated as an endpoint uri and resolved.

use std::any::Any;
use std::sync::Arc;

use crate::context::CamelContext;
use crate::error::{kinds, CamelError, Result};
use crate::spi::{Component, Endpoint};
use crate::util::Parameters;

#[derive(Default)]
pub struct RefComponent;

impl Component for RefComponent {
    fn create_endpoint(&self, ctx: &CamelContext, _uri: &str, remaining: &str, _params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let name = remaining.trim_start_matches('#');
        if let Some(e) = ctx.registry().lookup::<Arc<dyn Endpoint>>(name) {
            return Ok((*e).clone());
        }
        if let Some(uri) = ctx.registry().lookup::<String>(name) {
            return ctx.get_endpoint(&uri);
        }
        Err(CamelError::new(kinds::NO_SUCH_ENDPOINT, format!("No bean could be found in the registry for: {} of type: Endpoint", name)))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
