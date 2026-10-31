//! Shared plumbing of the Azure components: namespace resolution from connection strings
//! or shared access keys, AMQP connections authorised through CBS with SAS tokens.

use std::time::Duration;

use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_tls::camel_support::EndpointTls;

use crate::cbs::{put_token, RequestResponse, SAS_TOKEN_TYPE};
use crate::client::{Connection, ConnectionOptions, Session};
use crate::util::{now_secs, parse_connection_string, sas_token, Opts};

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
    /// Lifetime of generated SAS tokens.
    pub token_ttl: Duration,
    pub idle_timeout: Duration,
    pub request_timeout: Duration,
}

impl AzureNamespace {
    /// Resolve from `connectionString`, else `fullyQualifiedNamespace` / `namespace` with
    /// `sharedAccessName` / `sharedAccessKey`. Returns the namespace and the connection
    /// string's `EntityPath`.
    pub fn resolve(ctx: &CamelContext, scheme: &str, o: &mut Opts, namespace: Option<&str>) -> Result<(AzureNamespace, Option<String>)> {
        let cs = o.string("connectionString");
        let key_name_opt = o.string("sharedAccessName");
        let key_opt = o.string("sharedAccessKey").map(|k| k.replace(' ', "+"));
        let fqns = o.string("fullyQualifiedNamespace");
        let token_ttl = o.millis("sasTokenTtl", Duration::from_secs(3600))?;
        let idle_timeout = o.millis("idleTimeout", Duration::from_secs(60))?;
        let request_timeout = o.millis("requestTimeout", Duration::from_secs(30))?;
        o.ignore(&["credentialType", "amqpTransportType", "amqpRetryOptions", "clientOptions", "proxyOptions", "tokenCredential", "azureCredential"]);
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
                    _ => return Err(CamelError::illegal_argument("A connectionString, or a namespace with sharedAccessName and sharedAccessKey, must be set")),
                };
                (host, None, key_name_opt, key_opt, None, None, false)
            }
        };
        if sas.is_none() && (key_name.is_none() || key.is_none()) {
            return Err(CamelError::illegal_argument("Shared access key name and key (or a SharedAccessSignature) are required"));
        }
        let tls = camel_tls::camel_support::endpoint_tls(ctx, o.params, scheme, !emulator, &[])?;
        let port = port.unwrap_or(if tls.is_some() { 5671 } else { 5672 });
        Ok((AzureNamespace { host, port, tls, key_name, key, sas, token_ttl, idle_timeout, request_timeout }, entity))
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

    /// Open a connection (SASL ANONYMOUS) and a session, and put tokens for `entities` on `$cbs`.
    pub fn connect(&self, entities: &[&str]) -> Result<(Connection, Session)> {
        let mut o = ConnectionOptions::new(&self.host, self.port);
        o.tls = self.tls.clone();
        o.sasl_mechanism = Some("ANONYMOUS".into());
        o.idle_timeout = self.idle_timeout;
        o.request_timeout = self.request_timeout;
        let conn = Connection::open(&o)?;
        let session = conn.session()?;
        for e in entities {
            put_token(&session, &self.audience(e), &self.token(e), SAS_TOKEN_TYPE, self.request_timeout)?;
        }
        Ok((conn, session))
    }

    /// Open a request/response client on `node` (`entity/$management` or `$management`).
    pub fn management(&self, session: &Session, node: &str) -> Result<RequestResponse> {
        RequestResponse::open(session, node, self.request_timeout)
    }
}
