//! Attestation for integrations that run in a hardware TEE (`camel.attestation.*`, the
//! `attest` feature): Main builds the manifest of what runs, picks the evidence source, binds
//! the evidence to the embedded server's TLS key, and serves reports at `GET /q/attestation`.
//! The MCP server finds the service in the registry (`cameleer_core::attest`) and adds it to
//! its handshake and, when asked, as a tool.
//!
//! | property | default | |
//! |---|---|---|
//! | `camel.attestation.enabled` | `false` | produce evidence; start-up fails when no evidence source works |
//! | `camel.attestation.provider` | `auto` | `auto`, `tsm` (Linux configfs-tsm) or `dstack` |
//! | `camel.attestation.tsm-path` | `/sys/kernel/config/tsm/report` | |
//! | `camel.attestation.dstack-endpoint` | `/var/run/dstack.sock` | a unix socket or `http://host:port` (the simulator) |
//! | `camel.attestation.path` | `/q/attestation` | the HTTP path of reports |
//! | `camel.attestation.mcp-tool` | `false` | `true` (tool `cameleer_attestation`) or a tool name |
//! | `camel.attestation.exe-digest` | `true` | include the SHA-384 of the running executable |

use std::collections::BTreeMap;
use std::sync::Arc;

use cameleer_attest::{sha256_hex, sha384_hex, Attestation, Dstack, EvidenceProvider, Manifest, Tsm};
use cameleer_core::attest::AttestationService;
use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_http::http::{HttpRequest, HttpResponse};
use cameleer_http::registry::{registry, HttpRouteSpec, Registration, RouteHandler};
use cameleer_http::server::ServerConfig;

use crate::console::prop;

/// The default MCP tool name.
pub const DEFAULT_TOOL: &str = "cameleer_attestation";

/// A running attestation service and its HTTP registration.
pub struct AttestationHandle {
    registration: Option<Registration>,
}

impl Drop for AttestationHandle {
    fn drop(&mut self) {
        if let Some(r) = self.registration.take() {
            registry().unregister(&r);
        }
    }
}

fn flag(props: &BTreeMap<String, String>, k: &str, default: bool) -> bool {
    prop(props, k).map(|v| v.trim().eq_ignore_ascii_case("true")).unwrap_or(default)
}

/// The tool name when `camel.attestation.mcp-tool` turns the tool on.
pub fn tool_name(props: &BTreeMap<String, String>) -> Option<String> {
    match prop(props, "camel.attestation.mcp-tool").map(|v| v.trim().to_string()) {
        None => None,
        Some(v) if v.is_empty() || v.eq_ignore_ascii_case("false") => None,
        Some(v) if v.eq_ignore_ascii_case("true") => Some(DEFAULT_TOOL.to_string()),
        Some(v) => Some(v),
    }
}

/// Start attestation when `camel.attestation.enabled=true`.
pub fn start(ctx: &CamelContext, props: &BTreeMap<String, String>, route_files: &[std::path::PathBuf]) -> Result<Option<AttestationHandle>> {
    if !flag(props, "camel.attestation.enabled", false) {
        return Ok(None);
    }
    let provider = provider(props)?;
    let mut manifest = Manifest {
        version: env!("CARGO_PKG_VERSION").to_string(),
        features: crate::features::enabled().iter().map(|s| s.to_string()).collect(),
        target: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        ..Default::default()
    };
    if flag(props, "camel.attestation.exe-digest", true) {
        manifest.exe_sha384 = std::env::current_exe().ok().and_then(|p| std::fs::read(p).ok()).map(|b| format!("sha384:{}", sha384_hex(&b)));
    }
    for f in route_files {
        if let Ok(bytes) = std::fs::read(f) {
            manifest.route_files.insert(f.to_string_lossy().replace('\\', "/"), sha256_hex(&bytes));
        }
    }
    for r in ctx.routes() {
        manifest.routes.insert(r.id.clone(), (r.endpoint_uri.clone(), sha256_hex(r.definition_dump.as_bytes())));
    }
    manifest.set_properties(props);
    manifest.tools = tools(ctx);
    // the evidence is bound to the embedded server's TLS key, when it has one
    let tls_spki = cameleer_attest::server_tls_spki(ctx)?;
    let path = prop(props, "camel.attestation.path").unwrap_or_else(|| "/q/attestation".into());
    let weak = ctx.downgrade();
    let routes_fp = move || match weak.upgrade() {
        Some(ctx) => {
            let mut parts: Vec<String> = ctx.routes().iter().map(|r| format!("{}={}", r.id, sha256_hex(r.definition_dump.as_bytes()))).collect();
            parts.sort();
            format!("sha256:{}", sha256_hex(parts.join("\n").as_bytes()))
        }
        None => "stopped".into(),
    };
    let weak2 = ctx.downgrade();
    let tools_fp = move || match weak2.upgrade() {
        Some(ctx) => {
            let t = tools(&ctx);
            let text: Vec<String> = t.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
            format!("sha256:{}", sha256_hex(text.join("\n").as_bytes()))
        }
        None => "stopped".into(),
    };
    let platform = provider.platform();
    let service = Attestation::new(provider, manifest, tls_spki).with_endpoint(&path).with_tool(tool_name(props)).watch("routes", routes_fp).watch("tools", tools_fp);
    let service: Arc<dyn AttestationService> = Arc::new(service);
    // fail closed: a TEE that can't produce evidence must not look attested
    service.report(None).map_err(|e| CamelError::new("AttestationException", format!("camel.attestation.enabled=true but no evidence could be produced: {}", e.message)))?;
    cameleer_core::attest::bind(ctx, service.clone());
    let host = prop(props, "camel.server.host").unwrap_or_else(|| "0.0.0.0".into());
    let port: u16 = match prop(props, "camel.server.port") {
        Some(p) => p.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid camel.server.port: {}", p)))?,
        None => 8080,
    };
    let svc = service.clone();
    let handler: RouteHandler = Arc::new(move |req: &mut HttpRequest, _params: Vec<(String, String)>| handle(&svc, req));
    let spec = HttpRouteSpec { path: path.clone(), ..Default::default() };
    let registration = registry().register(&host, port, &ServerConfig { name: "camel-attestation".into(), ..Default::default() }, spec, handler)?;
    cameleer_core::camel_info!("Main", "Attestation enabled ({}): reports at {} on port {}", platform, path, registration.port);
    Ok(Some(AttestationHandle { registration: Some(registration) }))
}

fn provider(props: &BTreeMap<String, String>) -> Result<Box<dyn EvidenceProvider>> {
    let tsm = prop(props, "camel.attestation.tsm-path").unwrap_or_else(|| cameleer_attest::provider::TSM_DEFAULT.into());
    let dstack = prop(props, "camel.attestation.dstack-endpoint").unwrap_or_else(|| cameleer_attest::provider::DSTACK_DEFAULT.into());
    match prop(props, "camel.attestation.provider").map(|p| p.trim().to_ascii_lowercase()).as_deref().unwrap_or("auto") {
        "tsm" => Ok(Box::new(Tsm::new(tsm))),
        "dstack" => Ok(Box::new(Dstack::new(dstack))),
        "auto" => {
            // dstack first: on a dstack CVM its agent is the supported way in
            if Dstack::available(&dstack) && !dstack.starts_with("http://") {
                Ok(Box::new(Dstack::new(dstack)))
            } else if Tsm::available(std::path::Path::new(&tsm)) {
                Ok(Box::new(Tsm::new(tsm)))
            } else {
                Err(CamelError::new(
                    "AttestationException",
                    format!("camel.attestation.enabled=true but no evidence source was found: no dstack agent at {} and no configfs-tsm at {} (Linux 6.7+ in a TDX or SEV-SNP guest)", dstack, tsm),
                ))
            }
        }
        other => Err(CamelError::illegal_argument(format!("camel.attestation.provider must be auto, tsm or dstack, got {}", other))),
    }
}

/// Tool name → definition fingerprint of the published tools (and which are disabled).
#[cfg(feature = "ai")]
fn tools(ctx: &CamelContext) -> BTreeMap<String, String> {
    let Ok(reg) = cameleer_ai::AiRegistry::of(ctx) else { return BTreeMap::new() };
    let disabled = reg.disabled_tools();
    reg.all_tools()
        .iter()
        .map(|t| {
            let fp = t.definition_hash();
            (t.name.clone(), if disabled.contains(&t.name) { format!("{} disabled", fp) } else { fp })
        })
        .collect()
}

#[cfg(not(feature = "ai"))]
fn tools(_ctx: &CamelContext) -> BTreeMap<String, String> {
    BTreeMap::new()
}

fn handle(svc: &Arc<dyn AttestationService>, req: &HttpRequest) -> HttpResponse {
    if req.method != "GET" && req.method != "HEAD" {
        return HttpResponse::text(405, "Use GET").with_header("Allow", "GET, HEAD");
    }
    let nonce = match req.query_param("nonce") {
        None => None,
        // a '+' may arrive as a space in a query string
        Some(n) => match cameleer_attest::unb64(&n.replace(' ', "+")) {
            Some(b) => Some(b),
            None => return HttpResponse::text(400, "The nonce must be base64 or base64url"),
        },
    };
    match svc.report(nonce.as_deref()) {
        Ok(r) => HttpResponse::json(200, r.to_json()).with_header("Cache-Control", "no-store"),
        Err(e) if e.kind == cameleer_core::error::kinds::ILLEGAL_ARGUMENT => HttpResponse::text(400, e.message),
        Err(e) => HttpResponse::text(503, format!("Cannot produce evidence: {}", e.message)),
    }
}
