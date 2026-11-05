//! `twilio:` component (Camel `camel-twilio`): the Twilio REST API `2010-04-01` resources
//! that Camel exposes, as `twilio:<api>/<method>`.
//!
//! `twilio:message/create?from=+15005550006&to=+15005550001&username=AC...&password=token`
//!
//! APIs ([`RESOURCES`]): `account`, `address`, `address-dependent-phone-number`,
//! `application`, `available-phone-number-country` (+ `-local`, `-mobile`, `-toll-free`),
//! `call`, `call-feedback`, `call-feedback-summary`, `call-notification`, `call-recording`,
//! `conference`, `conference-participant`, `connect-app`, `incoming-phone-number` (+
//! `-local`, `-mobile`, `-toll-free`), `key`, `message`, `message-feedback`,
//! `message-media`, `new-key`, `new-signing-key`, `notification`, `outgoing-caller-id`,
//! `queue`, `queue-member`, `recording`, `recording-add-on-result`,
//! `recording-add-on-result-payload`, `recording-transcription`, `short-code`,
//! `signing-key`, `sip-credential-list`, `sip-credential-list-credential`, `sip-domain`,
//! `sip-domain-credential-list-mapping`, `sip-domain-ip-access-control-list-mapping`,
//! `sip-ip-access-control-list`, `sip-ip-access-control-list-ip-address`, `token`,
//! `transcription`, `usage-record` (+ `-all-time`, `-daily`, `-last-month`, `-monthly`,
//! `-this-month`, `-today`, `-yearly`, `-yesterday`), `usage-trigger`,
//! `validation-request`.
//!
//! Methods: `create`/`creator` (POST to the collection), `read`/`reader` (GET the
//! collection; the body becomes the resource list), `fetch`/`fetcher` (GET
//! `<collection>/<pathSid>`), `update`/`updater` (POST to the instance), `delete`/`deleter`
//! (DELETE the instance; the body becomes `true`).
//!
//! Parameters come from endpoint options or `CamelTwilio.<name>` headers (headers win).
//! `path*` parameters fill the resource path (`pathAccountSid` defaults to `accountSid`,
//! `pathSid` names the instance, parents such as `pathCallSid`, `pathConferenceSid`,
//! `pathQueueSid`, `pathMessageSid`, `pathCountryCode`...); every other parameter is sent
//! as a form field with its name capitalised the Twilio way (`statusCallback` →
//! `StatusCallback`, `to` → `To`); list values repeat the field. `inBody` names the
//! parameter taken from the message body (default `body` for `message/create` when no body
//! or `mediaUrl` parameter is set).
//!
//! Credentials: `username` (account SID) / `password` (auth token), `accountSid`
//! (defaults to the username). `serverUrl` (extension, default `https://api.twilio.com`)
//! overrides the API base url. Twilio errors fail with `HttpOperationFailedException`
//! containing the Twilio error code and message.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;

use crate::rest::{body_text, join_url, required, Req, Rest};

/// Prefix of the parameter headers.
pub const HEADER_PREFIX: &str = "CamelTwilio.";

/// A Twilio resource: API name, collection path below `/2010-04-01` (`{A}` =
/// `Accounts/{pathAccountSid}`, `{name}` = a `path<Name>` parameter), the parameter naming
/// the instance (`Sid` → `pathSid`), and the supported methods (c r f u d).
pub struct Resource {
    pub api: &'static str,
    pub path: &'static str,
    pub instance: Option<&'static str>,
    pub methods: &'static str,
}

const fn r(api: &'static str, path: &'static str, instance: Option<&'static str>, methods: &'static str) -> Resource {
    Resource { api, path, instance, methods }
}

const SID: Option<&str> = Some("Sid");

/// The resources Camel's `twilio` component exposes.
pub const RESOURCES: &[Resource] = &[
    r("account", "Accounts", SID, "crfu"),
    r("address", "{A}/Addresses", SID, "crfud"),
    r("address-dependent-phone-number", "{A}/Addresses/{AddressSid}/DependentPhoneNumbers", None, "r"),
    r("application", "{A}/Applications", SID, "crfud"),
    r("available-phone-number-country", "{A}/AvailablePhoneNumbers", Some("CountryCode"), "rf"),
    r("available-phone-number-country-local", "{A}/AvailablePhoneNumbers/{CountryCode}/Local", None, "r"),
    r("available-phone-number-country-mobile", "{A}/AvailablePhoneNumbers/{CountryCode}/Mobile", None, "r"),
    r("available-phone-number-country-toll-free", "{A}/AvailablePhoneNumbers/{CountryCode}/TollFree", None, "r"),
    r("call", "{A}/Calls", SID, "crfud"),
    r("call-feedback", "{A}/Calls/{CallSid}/Feedback", None, "cfu"),
    r("call-feedback-summary", "{A}/Calls/FeedbackSummary", SID, "cfd"),
    r("call-notification", "{A}/Calls/{CallSid}/Notifications", SID, "rf"),
    r("call-recording", "{A}/Calls/{CallSid}/Recordings", SID, "crfud"),
    r("conference", "{A}/Conferences", SID, "rfu"),
    r("conference-participant", "{A}/Conferences/{ConferenceSid}/Participants", Some("CallSid"), "crfud"),
    r("connect-app", "{A}/ConnectApps", SID, "rfud"),
    r("incoming-phone-number", "{A}/IncomingPhoneNumbers", SID, "crfud"),
    r("incoming-phone-number-local", "{A}/IncomingPhoneNumbers/Local", None, "cr"),
    r("incoming-phone-number-mobile", "{A}/IncomingPhoneNumbers/Mobile", None, "cr"),
    r("incoming-phone-number-toll-free", "{A}/IncomingPhoneNumbers/TollFree", None, "cr"),
    r("key", "{A}/Keys", SID, "rfud"),
    r("message", "{A}/Messages", SID, "crfud"),
    r("message-feedback", "{A}/Messages/{MessageSid}/Feedback", None, "c"),
    r("message-media", "{A}/Messages/{MessageSid}/Media", SID, "rfd"),
    r("new-key", "{A}/Keys", None, "c"),
    r("new-signing-key", "{A}/SigningKeys", None, "c"),
    r("notification", "{A}/Notifications", SID, "rf"),
    r("outgoing-caller-id", "{A}/OutgoingCallerIds", SID, "rfud"),
    r("queue", "{A}/Queues", SID, "crfud"),
    r("queue-member", "{A}/Queues/{QueueSid}/Members", Some("CallSid"), "rfu"),
    r("recording", "{A}/Recordings", SID, "rfd"),
    r("recording-add-on-result", "{A}/Recordings/{ReferenceSid}/AddOnResults", SID, "rfd"),
    r("recording-add-on-result-payload", "{A}/Recordings/{ReferenceSid}/AddOnResults/{AddOnResultSid}/Payloads", SID, "rfd"),
    r("recording-transcription", "{A}/Recordings/{RecordingSid}/Transcriptions", SID, "rfd"),
    r("short-code", "{A}/SMS/ShortCodes", SID, "rfu"),
    r("signing-key", "{A}/SigningKeys", SID, "rfud"),
    r("sip-credential-list", "{A}/SIP/CredentialLists", SID, "crfud"),
    r("sip-credential-list-credential", "{A}/SIP/CredentialLists/{CredentialListSid}/Credentials", SID, "crfud"),
    r("sip-domain", "{A}/SIP/Domains", SID, "crfud"),
    r("sip-domain-credential-list-mapping", "{A}/SIP/Domains/{DomainSid}/CredentialListMappings", SID, "crfd"),
    r("sip-domain-ip-access-control-list-mapping", "{A}/SIP/Domains/{DomainSid}/IpAccessControlListMappings", SID, "crfd"),
    r("sip-ip-access-control-list", "{A}/SIP/IpAccessControlLists", SID, "crfud"),
    r("sip-ip-access-control-list-ip-address", "{A}/SIP/IpAccessControlLists/{IpAccessControlListSid}/IpAddresses", SID, "crfud"),
    r("token", "{A}/Tokens", None, "c"),
    r("transcription", "{A}/Transcriptions", SID, "rfd"),
    r("usage-record", "{A}/Usage/Records", None, "r"),
    r("usage-record-all-time", "{A}/Usage/Records/AllTime", None, "r"),
    r("usage-record-daily", "{A}/Usage/Records/Daily", None, "r"),
    r("usage-record-last-month", "{A}/Usage/Records/LastMonth", None, "r"),
    r("usage-record-monthly", "{A}/Usage/Records/Monthly", None, "r"),
    r("usage-record-this-month", "{A}/Usage/Records/ThisMonth", None, "r"),
    r("usage-record-today", "{A}/Usage/Records/Today", None, "r"),
    r("usage-record-yearly", "{A}/Usage/Records/Yearly", None, "r"),
    r("usage-record-yesterday", "{A}/Usage/Records/Yesterday", None, "r"),
    r("usage-trigger", "{A}/Usage/Triggers", SID, "crfud"),
    r("validation-request", "{A}/OutgoingCallerIds", None, "c"),
];

/// The `twilio` component.
#[derive(Default)]
pub struct TwilioComponent;

#[derive(Clone, Debug)]
struct Config {
    api: &'static str,
    method: char,
    username: String,
    password: String,
    account_sid: String,
    server_url: String,
    in_body: Option<String>,
    params: BTreeMap<String, String>,
}

fn method_char(m: &str) -> Option<char> {
    Some(match m {
        "create" | "creator" => 'c',
        "read" | "reader" => 'r',
        "fetch" | "fetcher" => 'f',
        "update" | "updater" => 'u',
        "delete" | "deleter" => 'd',
        _ => return None,
    })
}

fn method_name(c: char) -> &'static str {
    match c {
        'c' => "create",
        'r' => "read",
        'f' => "fetch",
        'u' => "update",
        _ => "delete",
    }
}

/// Twilio form field name of a camelCase parameter (`statusCallback` → `StatusCallback`).
pub fn wire_name(param: &str) -> String {
    let mut c = param.chars();
    match c.next() {
        Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
        None => String::new(),
    }
}

fn resource(api: &str) -> Option<&'static Resource> {
    RESOURCES.iter().find(|r| r.api == api)
}

const ENDPOINT_OPTIONS: &[&str] = &["username", "password", "accountSid", "serverUrl", "inBody", "lazyStartProducer"];

impl Component for TwilioComponent {
    fn create_endpoint(&self, _ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let path = remaining.trim().trim_matches('/');
        let (api, m) = path.split_once('/').ok_or_else(|| CamelError::illegal_argument(format!("twilio: expected <api>/<method> but got '{}'", path)))?;
        let res = resource(api).ok_or_else(|| CamelError::illegal_argument(format!("twilio: unsupported api '{}' (one of {})", api, RESOURCES.iter().map(|r| r.api).collect::<Vec<_>>().join(", "))))?;
        let method = method_char(m).ok_or_else(|| CamelError::illegal_argument(format!("twilio: unknown method '{}' (create, read, fetch, update, delete)", m)))?;
        if !res.methods.contains(method) {
            let supported: Vec<&str> = res.methods.chars().map(method_name).collect();
            return Err(CamelError::illegal_argument(format!("twilio: api '{}' does not support method '{}' (supported: {})", api, m, supported.join(", "))));
        }
        let username = required(params, "username", "twilio")?;
        let password = required(params, "password", "twilio")?;
        let account_sid = params.take("accountSid").unwrap_or_else(|| username.clone());
        let server_url = params.take("serverUrl").unwrap_or_else(|| "https://api.twilio.com".into());
        let in_body = params.take("inBody");
        params.take("lazyStartProducer");
        // every remaining option is an API parameter
        let api_params: BTreeMap<String, String> = params.take_all().into_iter().filter(|(k, _)| !ENDPOINT_OPTIONS.contains(&k.as_str())).collect();
        let cfg = Config { api: res.api, method, username, password, account_sid, server_url, in_body, params: api_params };
        Ok(Arc::new(TwilioEndpoint { uri: uri.to_string(), cfg }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A twilio endpoint (producer only).
pub struct TwilioEndpoint {
    uri: String,
    cfg: Config,
}

impl Endpoint for TwilioEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(TwilioProducer { cfg: self.cfg.clone(), rest: Rest::default() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct TwilioProducer {
    cfg: Config,
    rest: Rest,
}

impl Service for TwilioProducer {}

/// Expand a resource path template with `path*` parameters.
fn expand(template: &str, p: &BTreeMap<String, Vec<String>>, account: &str) -> Result<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let end = rest[i..].find('}').map(|e| e + i).unwrap_or(rest.len());
        let name = &rest[i + 1..end];
        if name == "A" {
            out.push_str("Accounts/");
            out.push_str(&cameleer_core::util::url_encode(account));
        } else {
            let key = format!("path{}", name);
            let v = p.get(&key).and_then(|v| v.first()).ok_or_else(|| CamelError::illegal_argument(format!("twilio: missing parameter '{}'", key)))?;
            out.push_str(&cameleer_core::util::url_encode(v));
        }
        rest = &rest[(end + 1).min(rest.len())..];
    }
    out.push_str(rest);
    Ok(out)
}

impl Processor for TwilioProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let res = resource(self.cfg.api).expect("validated api");
        let mut p: BTreeMap<String, Vec<String>> = self.cfg.params.iter().map(|(k, v)| (k.clone(), vec![v.clone()])).collect();
        for (k, v) in ex.message.headers.iter() {
            let Some(name) = k.strip_prefix(HEADER_PREFIX) else { continue };
            let vals: Vec<String> = match v {
                Value::Null => continue,
                Value::Array(a) => a.iter().map(|x| x.to_string()).collect(),
                other => vec![other.to_string()],
            };
            p.insert(name.to_string(), vals);
        }
        let in_body = self.cfg.in_body.clone().or_else(|| if res.api == "message" && self.cfg.method == 'c' && !p.contains_key("body") && !p.contains_key("mediaUrl") { Some("body".into()) } else { None });
        if let Some(name) = in_body {
            match &ex.message.body {
                Value::Array(a) => {
                    p.insert(name, a.iter().map(|x| x.to_string()).collect());
                }
                _ => {
                    if let Some(b) = body_text(ex) {
                        p.insert(name, vec![b]);
                    }
                }
            }
        }
        let account = p.get("pathAccountSid").and_then(|v| v.first().cloned()).unwrap_or_else(|| self.cfg.account_sid.clone());
        let collection = expand(res.path, &p, &account)?;
        let base = join_url(&self.cfg.server_url, &format!("/2010-04-01/{}", collection));
        let instance_url = || -> Result<String> {
            match res.instance {
                Some(name) => {
                    let key = format!("path{}", name);
                    let sid = p.get(&key).and_then(|v| v.first()).ok_or_else(|| CamelError::illegal_argument(format!("twilio: {}/{} requires the '{}' parameter", res.api, method_name(self.cfg.method), key)))?;
                    Ok(format!("{}/{}.json", base, cameleer_core::util::url_encode(sid)))
                }
                None => Ok(format!("{}.json", base)),
            }
        };
        let form = || -> Vec<(String, String)> { p.iter().filter(|(k, _)| !k.starts_with("path")).flat_map(|(k, vs)| vs.iter().map(move |v| (wire_name(k), v.clone()))).collect() };
        let req = match self.cfg.method {
            'c' => {
                let f = form();
                if res.api == "message" && !f.iter().any(|(k, _)| k == "To") {
                    return Err(CamelError::illegal_argument("twilio: message/create requires the 'to' parameter"));
                }
                Req::post(format!("{}.json", base)).form(&f.iter().map(|(k, v)| (k.as_str(), v.clone())).collect::<Vec<_>>())
            }
            'r' => {
                let f = form();
                let q = crate::rest::query(&f.iter().map(|(k, v)| (k.as_str(), v.clone())).collect::<Vec<_>>());
                Req::get(if q.is_empty() { format!("{}.json", base) } else { format!("{}.json?{}", base, q) })
            }
            'f' => Req::get(instance_url()?),
            'u' => {
                let f = form();
                Req::post(instance_url()?).form(&f.iter().map(|(k, v)| (k.as_str(), v.clone())).collect::<Vec<_>>())
            }
            _ => Req::new("DELETE", instance_url()?),
        };
        let resp = self.rest.send(req.basic(&self.cfg.username, &self.cfg.password))?;
        if !resp.is_success() {
            let v = resp.json().unwrap_or(Value::Null);
            let msg = format!(
                "HTTP operation failed invoking Twilio {}/{} with statusCode: {}, code: {}, message: {}",
                res.api,
                method_name(self.cfg.method),
                resp.status,
                v.get("code").map(|c| c.to_string()).unwrap_or_default(),
                v.get("message").map(|c| c.to_string()).unwrap_or_else(|| resp.text())
            );
            return Err(CamelError::new(cameleer_core::error::kinds::HTTP_OPERATION_FAILED, msg));
        }
        ex.message.body = match self.cfg.method {
            'd' => Value::Bool(true),
            'r' => {
                let v = resp.json()?;
                // the list is the (single) array member of the page object, besides `meta`
                let list = v.as_object().and_then(|o| o.iter().find(|(k, x)| k.as_str() != "meta" && x.as_array().is_some()).map(|(_, x)| x.clone()));
                list.unwrap_or(v)
            }
            _ => resp.json()?,
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_paths() {
        assert_eq!(wire_name("statusCallback"), "StatusCallback");
        assert_eq!(wire_name("to"), "To");
        let mut p = BTreeMap::new();
        p.insert("pathCallSid".to_string(), vec!["CA1".to_string()]);
        assert_eq!(expand("{A}/Calls/{CallSid}/Recordings", &p, "AC9").unwrap(), "Accounts/AC9/Calls/CA1/Recordings");
        assert!(expand("{A}/Queues/{QueueSid}/Members", &p, "AC9").is_err());
        assert!(RESOURCES.iter().all(|r| r.instance.is_some() || !r.methods.contains('d')));
    }
}
