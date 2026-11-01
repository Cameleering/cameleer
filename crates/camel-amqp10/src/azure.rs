//! Shared plumbing of the Azure components: namespace resolution from connection strings,
//! shared access keys or Azure AD credentials, AMQP connections (directly or over
//! WebSockets) authorised through CBS with SAS or `jwt` tokens that are refreshed before
//! they expire.
//!
//! Options: `connectionString` or `fullyQualifiedNamespace` (or the namespace in the uri)
//! with `sharedAccessName` / `sharedAccessKey`; `credentialType` `CONNECTION_STRING`
//! (default), `AZURE_IDENTITY` ([`crate::identity::DefaultAzureCredential`], or a client
//! secret / managed identity from the rust-camel options `tenantId`, `clientId`,
//! `clientSecret`, `authorityHost`, `managedIdentityEndpoint`, `managedIdentityClientId`)
//! or `TOKEN_CREDENTIAL` (`tokenCredential=#bean` with an `Arc<dyn TokenCredential>`);
//! `amqpTransportType` `AMQP` (default) or `AMQP_WEB_SOCKETS` (port 443, path
//! `/$servicebus/websocket`); `sasTokenTtl`, `idleTimeout`, `requestTimeout`.

use std::sync::Arc;
use std::time::Duration;

use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_tls::camel_support::EndpointTls;

use crate::cbs::{put_token, RequestResponse, JWT_TOKEN_TYPE, SAS_TOKEN_TYPE};
use crate::client::{Connection, ConnectionOptions, Session};
use crate::identity::{ClientSecretCredential, Credential, DefaultAzureCredential, ManagedIdentityCredential, TokenCredential};
use crate::util::{now_secs, parse_connection_string, sas_token, Opts};

/// Service Bus session filter (`com.microsoft:session-filter`, a source filter whose
/// value is the session id to lock, or null for the next available session).
pub const SESSION_FILTER: &str = "com.microsoft:session-filter";
/// Numeric descriptor of the session filter.
pub const SESSION_FILTER_CODE: u64 = 0x0000_0137_0000_000C;
/// .NET `DateTime` ticks (100 ns since 0001-01-01) at the Unix epoch.
const TICKS_AT_UNIX_EPOCH: i64 = 621_355_968_000_000_000;

/// Unix milliseconds as .NET ticks (Service Bus `com.microsoft:locked-until-utc`).
pub fn millis_to_ticks(millis: i64) -> i64 {
    millis * 10_000 + TICKS_AT_UNIX_EPOCH
}

/// The `com.microsoft:locked-until-utc` attach property in Unix milliseconds: Service Bus sends
/// .NET ticks (a long); an AMQP timestamp is accepted too.
pub fn locked_until_millis(v: &crate::codec::AmqpValue) -> i64 {
    match v {
        crate::codec::AmqpValue::Timestamp(ms) => *ms,
        other => match other.as_i64() {
            Some(t) if t > TICKS_AT_UNIX_EPOCH => (t - TICKS_AT_UNIX_EPOCH) / 10_000,
            Some(t) => t,
            None => 0,
        },
    }
}

/// Event Hubs receiver epoch (owner level) link property.
pub const EPOCH_PROPERTY: &str = "com.microsoft:epoch";

/// How to reach and authenticate against an Azure messaging namespace.
#[derive(Clone, Debug)]
pub struct AzureNamespace {
    /// Fully qualified namespace host (`ns.servicebus.windows.net`).
    pub host: String,
    pub port: u16,
    /// TLS settings (`None` for the development emulator).
    pub tls: Option<EndpointTls>,
    pub key_name: Option<String>,
    pub key: Option<String>,
    /// A ready SAS token (instead of a key).
    pub sas: Option<String>,
    /// An Azure AD credential (`jwt` tokens) instead of SAS.
    pub credential: Option<Credential>,
    /// The token scope of the credential (Service Bus or Event Hubs).
    pub scope: String,
    /// AMQP over WebSockets.
    pub websocket: bool,
    /// Lifetime of generated SAS tokens.
    pub token_ttl: Duration,
    pub idle_timeout: Duration,
    pub request_timeout: Duration,
}

impl AzureNamespace {
    /// Resolve from `connectionString`, else `fullyQualifiedNamespace` / `namespace` with
    /// `sharedAccessName` / `sharedAccessKey` or an Azure AD credential. Returns the
    /// namespace and the connection string's `EntityPath`. `scope` is the token scope
    /// of Azure AD credentials.
    pub fn resolve(ctx: &CamelContext, scheme: &str, o: &mut Opts, namespace: Option<&str>, scope: &str) -> Result<(AzureNamespace, Option<String>)> {
        let cs = o.string("connectionString");
        let key_name_opt = o.string("sharedAccessName");
        let key_opt = o.string("sharedAccessKey").map(|k| k.replace(' ', "+"));
        let fqns = o.string("fullyQualifiedNamespace");
        let token_ttl = o.millis("sasTokenTtl", Duration::from_secs(3600))?;
        let idle_timeout = o.millis("idleTimeout", Duration::from_secs(60))?;
        let request_timeout = o.millis("requestTimeout", Duration::from_secs(30))?;
        let credential_type = o.string("credentialType").map(|s| s.trim().to_ascii_uppercase());
        let websocket = match o.string("amqpTransportType").map(|s| s.trim().to_ascii_uppercase().replace('_', "")) {
            None => false,
            Some(t) if t == "AMQP" => false,
            Some(t) if t == "AMQPWEBSOCKETS" => true,
            Some(t) => return Err(CamelError::illegal_argument(format!("Invalid amqpTransportType (AMQP or AMQP_WEB_SOCKETS): {}", t))),
        };
        let token_bean = o.string("tokenCredential").or_else(|| o.string("azureCredential"));
        let tenant = o.string("tenantId");
        let client_id = o.string("clientId");
        let client_secret = o.string("clientSecret");
        let authority = o.string("authorityHost");
        let mi_endpoint = o.string("managedIdentityEndpoint");
        let mi_client = o.string("managedIdentityClientId");
        o.ignore(&["amqpRetryOptions", "clientOptions", "proxyOptions"]);
        let (host, port, key_name, key, sas, entity, emulator) = match cs {
            Some(cs) => {
                let c = parse_connection_string(&cs)?;
                (c.host, c.port, c.key_name.or(key_name_opt), c.key.or(key_opt), c.sas, c.entity_path, c.emulator)
            }
            None => {
                let host = match (fqns, namespace) {
                    (Some(f), _) => f,
                    (None, Some(ns)) if ns.contains('.') => ns.to_string(),
                    (None, Some(ns)) if !ns.is_empty() => format!("{}.servicebus.windows.net", ns),
                    _ => return Err(CamelError::illegal_argument("A connectionString, or a namespace with sharedAccessName and sharedAccessKey or credentials, must be set")),
                };
                let (h, p) = match host.rsplit_once(':') {
                    Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => (h.to_string(), p.parse().ok()),
                    _ => (host, None),
                };
                (h, p, key_name_opt, key_opt, None, None, false)
            }
        };
        let tls = camel_tls::camel_support::endpoint_tls(ctx, o.params, scheme, !emulator, &[])?;
        let http_tls = match &tls {
            Some(t) => Some(t.client_config()?),
            None => None,
        };
        let credential: Option<Arc<dyn TokenCredential>> = match credential_type.as_deref() {
            None | Some("CONNECTION_STRING") | Some("SHARED_ACCESS_KEY") => None,
            Some("TOKEN_CREDENTIAL") => {
                let r = token_bean.ok_or_else(|| CamelError::illegal_argument("credentialType=TOKEN_CREDENTIAL needs tokenCredential=#bean"))?;
                let name = r.trim_start_matches('#');
                let bean = ctx.registry().lookup::<Arc<dyn TokenCredential>>(name).ok_or_else(|| CamelError::new(camel_core::error::kinds::NO_SUCH_BEAN, format!("No TokenCredential bean named {}", name)))?;
                Some((*bean).clone())
            }
            Some("AZURE_IDENTITY") => Some(match (tenant, client_id.clone(), client_secret, mi_endpoint) {
                (Some(t), Some(c), Some(s), _) => {
                    let mut cred = ClientSecretCredential::new(&t, &c, &s);
                    if let Some(a) = &authority {
                        cred = cred.authority_host(a);
                    }
                    if let Some(cfg) = &http_tls {
                        cred = cred.tls(cfg.clone());
                    }
                    Arc::new(cred)
                }
                (_, _, _, Some(ep)) => {
                    let mut cred = ManagedIdentityCredential::new(mi_client.as_deref().or(client_id.as_deref())).endpoint(&ep);
                    if let Some(cfg) = &http_tls {
                        cred = cred.tls(cfg.clone());
                    }
                    Arc::new(cred)
                }
                _ if mi_client.is_some() => Arc::new(ManagedIdentityCredential::new(mi_client.as_deref())),
                _ => Arc::new(DefaultAzureCredential::new(http_tls.clone())),
            }),
            Some(other) => return Err(CamelError::illegal_argument(format!("Invalid credentialType (CONNECTION_STRING, AZURE_IDENTITY or TOKEN_CREDENTIAL): {}", other))),
        };
        if credential.is_none() && sas.is_none() && (key_name.is_none() || key.is_none()) {
            return Err(CamelError::illegal_argument("Shared access key name and key (or a SharedAccessSignature, or credentialType=AZURE_IDENTITY / TOKEN_CREDENTIAL) are required"));
        }
        let port = port.unwrap_or(match (websocket, tls.is_some()) {
            (true, true) => 443,
            (true, false) => 80,
            (false, true) => 5671,
            (false, false) => 5672,
        });
        let ns = AzureNamespace { host, port, tls, key_name, key, sas, credential: credential.map(Credential), scope: scope.to_string(), websocket, token_ttl, idle_timeout, request_timeout };
        Ok((ns, entity))
    }

    /// The audience (resource uri) of an entity: `sb://host/entity`.
    pub fn audience(&self, entity: &str) -> String {
        format!("sb://{}/{}", self.host, entity.trim_matches('/'))
    }

    /// A SAS token for an entity.
    pub fn token(&self, entity: &str) -> String {
        if let Some(s) = &self.sas {
            return s.clone();
        }
        sas_token(&self.audience(entity), self.key_name.as_deref().unwrap_or(""), self.key.as_deref().unwrap_or(""), now_secs() + self.token_ttl.as_secs().max(60))
    }

    /// The CBS token for an entity: (token, token type, expiry in epoch seconds).
    pub fn cbs_token(&self, entity: &str) -> Result<(String, &'static str, u64)> {
        match &self.credential {
            Some(c) => {
                let t = c.0.get_token(&self.scope)?;
                Ok((t.token, JWT_TOKEN_TYPE, t.expires_on))
            }
            None => {
                let expiry = if self.sas.is_some() { now_secs() + self.token_ttl.as_secs() } else { now_secs() + self.token_ttl.as_secs().max(60) };
                Ok((self.token(entity), SAS_TOKEN_TYPE, expiry))
            }
        }
    }

    /// Open a connection (SASL ANONYMOUS) and a session, and put tokens for `entities` on
    /// `$cbs`. A background thread puts fresh tokens when 80 % of their lifetime passed,
    /// for as long as the session is open.
    pub fn connect(&self, entities: &[&str]) -> Result<(Connection, Session)> {
        let mut o = ConnectionOptions::new(&self.host, self.port);
        o.tls = self.tls.clone();
        o.sasl_mechanism = Some("ANONYMOUS".into());
        o.idle_timeout = self.idle_timeout;
        o.request_timeout = self.request_timeout;
        if self.websocket {
            o.websocket_path = Some(crate::ws::AMQP_WS_PATH.to_string());
        }
        let conn = Connection::open(&o)?;
        let session = conn.session()?;
        let mut expiry = u64::MAX;
        for e in entities {
            let (token, ty, exp) = self.cbs_token(e)?;
            put_token(&session, &self.audience(e), &token, ty, self.request_timeout)?;
            expiry = expiry.min(exp);
        }
        if !entities.is_empty() {
            self.spawn_refresh(&session, entities.iter().map(|e| e.to_string()).collect(), expiry);
        }
        Ok((conn, session))
    }

    fn spawn_refresh(&self, session: &Session, entities: Vec<String>, mut expiry: u64) {
        let (ns, session) = (self.clone(), session.clone());
        let _ = std::thread::Builder::new().name(format!("azure-cbs-refresh-{}", self.host)).spawn(move || {
            let mut issued = crate::util::now_millis();
            loop {
                let lifetime_ms = (expiry.saturating_mul(1000) as i64 - issued).max(1000);
                let due = issued + lifetime_ms * 8 / 10;
                while crate::util::now_millis() < due {
                    if !session.is_open() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                issued = crate::util::now_millis();
                let mut next = u64::MAX;
                for e in &entities {
                    match ns.cbs_token(e).and_then(|(token, ty, exp)| put_token(&session, &ns.audience(e), &token, ty, ns.request_timeout).map(|_| exp)) {
                        Ok(exp) => next = next.min(exp),
                        Err(err) => {
                            if !session.is_open() {
                                return;
                            }
                            camel_core::camel_warn!("azure", "Cannot refresh the CBS token of {}: {}", e, err.message);
                            next = next.min(now_secs() + 5);
                        }
                    }
                }
                expiry = next;
            }
        });
    }

    /// Open a request/response client on `node` (`entity/$management` or `$management`).
    pub fn management(&self, session: &Session, node: &str) -> Result<RequestResponse> {
        RequestResponse::open(session, node, self.request_timeout)
    }
}
