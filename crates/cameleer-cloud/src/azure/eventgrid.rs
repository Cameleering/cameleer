//! The `azure-eventgrid` component (Camel `camel-azure-eventgrid`, producer): publishes events
//! to an Event Grid topic.
//!
//! `azure-eventgrid:https://mytopic.westeurope-1.eventgrid.azure.net/api/events?accessKey=RAW(...)`
//!
//! Schemas (`eventSchema`): `CloudEvent` (default, CloudEvents 1.0 JSON batch,
//! `application/cloudevents-batch+json`) or `EventGrid` (Event Grid schema). The body is the
//! event `data` (a list body publishes one event per element); `CamelAzureEventGridEventType`
//! (default `org.apache.camel.event`), `...Subject`, `...Source` (CloudEvents; default the
//! topic endpoint), `...EventId` (default random UUID), `...DataVersion` (Event Grid schema,
//! default `1.0`), `...DataContentType` (CloudEvents, default `application/json`). The ids
//! sent are set in `CamelAzureEventGridEventId` (a list for list bodies).
//!
//! Authorization (`credentialType`): `ACCESS_KEY` (default with `accessKey`: `aeg-sas-key`
//! header), `AZURE_SAS` (an `aeg-sas-token` header: the `sharedAccessSignature` option, or a
//! token signed with `accessKey` valid for `sasTokenTtl` seconds, default 3600) or Entra ID
//! client credentials (`tenantId`, `clientId`, `clientSecret`, `authorityHost`;
//! `AZURE_IDENTITY` from `AZURE_*` env vars; scope `https://eventgrid.azure.net/.default`).

use std::sync::Arc;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::rest::{aad_credential, credential_type, AzureAuth, AzureRest};
use crate::endpoint::CloudService;
use crate::http::Request;
use crate::util::{base64_decode, base64_encode, header_str, hmac_sha256, iso8601_millis, kinds, now_millis, now_secs, uri_encode, uuid, Opts};

/// Header names (Camel `EventGridConstants`).
pub mod headers {
    pub const EVENT_TYPE: &str = "CamelAzureEventGridEventType";
    pub const SUBJECT: &str = "CamelAzureEventGridSubject";
    pub const SOURCE: &str = "CamelAzureEventGridSource";
    pub const EVENT_ID: &str = "CamelAzureEventGridEventId";
    pub const DATA_VERSION: &str = "CamelAzureEventGridDataVersion";
    pub const DATA_CONTENT_TYPE: &str = "CamelAzureEventGridDataContentType";
}

/// The Event Grid scope for Entra ID tokens.
pub const SCOPE: &str = "https://eventgrid.azure.net/.default";

/// The Event Grid endpoint configuration and client.
pub struct EventGridService {
    pub rest: AzureRest,
    pub topic_endpoint: String,
    pub cloud_events: bool,
    /// SAS generation: (key, ttl seconds).
    sas: Option<(String, i64)>,
}

/// An Event Grid SAS token (`r=...&e=...&s=...`) for `resource` expiring at `expiry`
/// (`M/d/yyyy h:mm:ss AM` is what the SDKs send; any string is signed as is).
pub fn sas_token(resource: &str, expiry: &str, key_b64: &str) -> Result<String> {
    let key = base64_decode(key_b64).ok_or_else(|| CamelError::illegal_argument("accessKey must be base64"))?;
    let unsigned = format!("r={}&e={}", uri_encode(resource, false), uri_encode(expiry, false));
    let sig = base64_encode(&hmac_sha256(&key, unsigned.as_bytes()));
    Ok(format!("{}&s={}", unsigned, uri_encode(&sig, false)))
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let topic = match remaining {
        r if r.starts_with("http://") || r.starts_with("https://") => r.to_string(),
        r if r.starts_with("http:/") || r.starts_with("https:/") => r.replacen(":/", "://", 1),
        "" => o.get("topicEndpoint").ok_or_else(|| CamelError::illegal_argument("Expected azure-eventgrid:<topic endpoint URL>"))?,
        r => format!("https://{}", r),
    };
    let schema = o.string("eventSchema", "CloudEvent");
    let cloud_events = match schema.to_ascii_lowercase().as_str() {
        "cloudevent" | "cloudevents" => true,
        "eventgrid" | "eventgridevent" => false,
        other => return Err(CamelError::illegal_argument(format!("Invalid eventSchema: {} (CloudEvent or EventGrid)", other))),
    };
    let key = o.get("accessKey");
    let ct = credential_type(o, if key.is_some() { "ACCESS_KEY" } else { "AZURE_IDENTITY" }, &["ACCESS_KEY", "AZURE_SAS", "AZURE_IDENTITY", "CLIENT_SECRET", "TOKEN_CREDENTIAL"])?;
    let sas_given = o.get("sharedAccessSignature");
    let ttl = o.i64("sasTokenTtl", 3600)?;
    let mut sas = None;
    let auth = match ct.as_str() {
        "ACCESS_KEY" => AzureAuth::Header("aeg-sas-key".into(), key.ok_or_else(|| CamelError::illegal_argument("credentialType=ACCESS_KEY requires accessKey"))?),
        "AZURE_SAS" => match (sas_given, key) {
            (Some(s), _) => AzureAuth::Header("aeg-sas-token".into(), s),
            (None, Some(k)) => {
                sas = Some((k, ttl));
                AzureAuth::None
            }
            (None, None) => return Err(CamelError::illegal_argument("credentialType=AZURE_SAS requires sharedAccessSignature or accessKey")),
        },
        _ => {
            let c = aad_credential(o, ct == "AZURE_IDENTITY")?.ok_or_else(|| CamelError::illegal_argument("Entra ID authentication requires tenantId / clientId / clientSecret"))?;
            AzureAuth::Bearer(c, SCOPE.into())
        }
    };
    let rest = AzureRest::from_opts(o, &topic, auth, kinds::AZURE_HTTP)?;
    o.ignore(&["publisherClient", "tokenCredential"]);
    Ok(Arc::new(EventGridService { rest, topic_endpoint: topic, cloud_events, sas }))
}

impl EventGridService {
    fn event(&self, ex: &Exchange, data: &Value, id: &str) -> Value {
        let mut m = ObjectMap::new();
        let ty = header_str(ex, headers::EVENT_TYPE).unwrap_or_else(|| "org.apache.camel.event".into());
        let subject = header_str(ex, headers::SUBJECT);
        let data = match data {
            Value::Bytes(b) => match cameleer_core::json::parse(&String::from_utf8_lossy(b)) {
                Ok(v) => v,
                Err(_) => Value::from(String::from_utf8_lossy(b).into_owned()),
            },
            other => other.clone(),
        };
        if self.cloud_events {
            m.insert("specversion".into(), Value::from("1.0"));
            m.insert("id".into(), Value::from(id));
            m.insert("source".into(), Value::from(header_str(ex, headers::SOURCE).unwrap_or_else(|| self.topic_endpoint.clone())));
            m.insert("type".into(), Value::from(ty));
            if let Some(s) = subject {
                m.insert("subject".into(), Value::from(s));
            }
            m.insert("time".into(), Value::from(iso8601_millis(now_millis())));
            m.insert("datacontenttype".into(), Value::from(header_str(ex, headers::DATA_CONTENT_TYPE).unwrap_or_else(|| "application/json".into())));
            m.insert("data".into(), data);
        } else {
            m.insert("id".into(), Value::from(id));
            m.insert("subject".into(), Value::from(subject.unwrap_or_else(|| "camel".into())));
            m.insert("eventType".into(), Value::from(ty));
            m.insert("eventTime".into(), Value::from(iso8601_millis(now_millis())));
            m.insert("dataVersion".into(), Value::from(header_str(ex, headers::DATA_VERSION).unwrap_or_else(|| "1.0".into())));
            m.insert("data".into(), data);
        }
        Value::Object(m)
    }
}

impl CloudService for EventGridService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let (items, is_list) = match ex.body() {
            Value::Array(a) => (a.clone(), true),
            other => (vec![other.clone()], false),
        };
        let given_id = header_str(ex, headers::EVENT_ID);
        let ids: Vec<String> = (0..items.len()).map(|i| if i == 0 && !is_list { given_id.clone().unwrap_or_else(uuid) } else { uuid() }).collect();
        let events: Vec<Value> = items.iter().zip(ids.iter()).map(|(d, id)| self.event(ex, d, id)).collect();
        let ct = if self.cloud_events { "application/cloudevents-batch+json; charset=utf-8" } else { "application/json" };
        let mut req = Request::new("POST", self.rest.url("", "api-version=2018-01-01")?).header("Content-Type", ct).body(Value::Array(events).to_json().into_bytes());
        if let Some((key, ttl)) = &self.sas {
            let expiry = crate::util::iso8601(now_secs() + ttl);
            req.set_header("aeg-sas-token", sas_token(&self.topic_endpoint, &expiry, key)?);
        }
        self.rest.send(req)?;
        if is_list {
            ex.set_header(headers::EVENT_ID, Value::from(ids));
        } else {
            ex.set_header(headers::EVENT_ID, ids.into_iter().next().unwrap_or_default());
        }
        Ok(())
    }
}
