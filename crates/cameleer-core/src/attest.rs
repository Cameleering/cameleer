//! Attestation SPI: when Cameleer runs in a hardware TEE (a confidential VM or an enclave),
//! the `cameleer-attest` crate binds an [`AttestationService`] in the registry, and the
//! surfaces that hand evidence to clients (the `/q/attestation` endpoint, the MCP server's
//! `_meta` and its attestation tool) look it up here. Keeping the trait in the core lets those
//! surfaces work without depending on the crate that produces the evidence.

use std::sync::Arc;

use crate::context::CamelContext;
use crate::error::Result;
use crate::value::Value;

/// The registry name of the [`AttestationService`].
pub const ATTESTATION_BEAN: &str = "cameleer.attestation";

/// Produces attestation evidence for this process.
pub trait AttestationService: Send + Sync {
    /// What a client needs to know before asking for evidence: the platform, the evidence
    /// format, the current manifest digest, the TLS key hash the evidence is bound to, and where
    /// to fetch reports.
    fn summary(&self) -> Value;
    /// Fresh evidence bound to `nonce` (16 to 64 bytes chosen by the client; `None` makes the
    /// service pick a random one, which proves nothing about freshness to the client).
    fn report(&self, nonce: Option<&[u8]>) -> Result<Value>;
}

/// The attestation service bound in `ctx`, if attestation is enabled.
pub fn service(ctx: &CamelContext) -> Option<Arc<dyn AttestationService>> {
    ctx.registry().lookup::<Arc<dyn AttestationService>>(ATTESTATION_BEAN).map(|a| (*a).clone())
}

/// Bind `service` as the context's attestation service.
pub fn bind(ctx: &CamelContext, service: Arc<dyn AttestationService>) {
    ctx.registry().bind(ATTESTATION_BEAN, service);
}
