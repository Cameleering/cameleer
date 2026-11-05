//! WS-Addressing 1.0 message addressing properties and WS-ReliableMessaging 1.1 basics.
//!
//! WS-Addressing: requests carry `wsa:Action`, `wsa:To`, `wsa:MessageID` and
//! `wsa:ReplyTo`; responses carry `wsa:Action` (`...Response`), `wsa:RelatesTo` and
//! `wsa:MessageID`. A non-anonymous `ReplyTo` makes the service answer `202` and send the
//! response to that address (decoupled endpoint).
//!
//! WS-RM: the client creates a sequence (`CreateSequence` / `CreateSequenceResponse`),
//! numbers its messages with a `wsrm:Sequence` header and terminates the sequence when
//! stopped; the server acknowledges every message (`wsrm:SequenceAcknowledgement`) and
//! answers a duplicate message number with the cached response instead of processing it
//! again.

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use cameleer_xml::dom::Document;
use cameleer_xml::serialize::escape_text;

pub const WSA_NS: &str = "http://www.w3.org/2005/08/addressing";
pub const WSRM_NS: &str = "http://docs.oasis-open.org/ws-rx/wsrm/200702";
pub const ANONYMOUS: &str = "http://www.w3.org/2005/08/addressing/anonymous";
pub const CREATE_SEQUENCE: &str = "http://docs.oasis-open.org/ws-rx/wsrm/200702/CreateSequence";
pub const CREATE_SEQUENCE_RESPONSE: &str = "http://docs.oasis-open.org/ws-rx/wsrm/200702/CreateSequenceResponse";
pub const TERMINATE_SEQUENCE: &str = "http://docs.oasis-open.org/ws-rx/wsrm/200702/TerminateSequence";
pub const TERMINATE_SEQUENCE_RESPONSE: &str = "http://docs.oasis-open.org/ws-rx/wsrm/200702/TerminateSequenceResponse";

/// Message addressing properties of a message.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Addressing {
    pub action: Option<String>,
    pub to: Option<String>,
    pub message_id: Option<String>,
    pub reply_to: Option<String>,
    pub relates_to: Option<String>,
    /// `wsrm:Sequence` identifier and message number.
    pub sequence: Option<(String, u64)>,
}

fn el(local: &str, text: &str) -> String {
    format!("<wsa:{l} xmlns:wsa=\"{ns}\">{t}</wsa:{l}>", l = local, ns = WSA_NS, t = escape_text(text))
}

pub fn new_message_id() -> String {
    format!("urn:uuid:{}", cameleer_core::message::generate_id())
}

/// Request headers.
pub fn request_headers(action: &str, to: &str, message_id: &str, reply_to: Option<&str>) -> Vec<String> {
    vec![
        el("Action", action),
        el("MessageID", message_id),
        el("To", to),
        format!("<wsa:ReplyTo xmlns:wsa=\"{ns}\"><wsa:Address>{a}</wsa:Address></wsa:ReplyTo>", ns = WSA_NS, a = escape_text(reply_to.unwrap_or(ANONYMOUS))),
    ]
}

/// Response headers for a request.
pub fn reply_headers(req: &Addressing, action: &str) -> Vec<String> {
    let mut h = vec![el("Action", action), el("MessageID", &new_message_id()), el("To", req.reply_to.as_deref().unwrap_or(ANONYMOUS))];
    if let Some(id) = &req.message_id {
        h.push(el("RelatesTo", id));
    }
    h
}

fn text_of(d: &Document, n: cameleer_xml::dom::NodeId) -> String {
    d.string_value(n).trim().to_string()
}

/// Read the addressing (and RM sequence) properties from SOAP header blocks.
pub fn parse(headers: &[String]) -> Addressing {
    let mut a = Addressing::default();
    for h in headers {
        let Ok(d) = Document::parse(h) else { continue };
        let Some(r) = d.document_element() else { continue };
        let ns = d.namespace_uri(r);
        let local = d.local_name(r);
        if ns == WSA_NS {
            match local {
                "Action" => a.action = Some(text_of(&d, r)),
                "To" => a.to = Some(text_of(&d, r)),
                "MessageID" => a.message_id = Some(text_of(&d, r)),
                "RelatesTo" => a.relates_to = Some(text_of(&d, r)),
                "ReplyTo" => a.reply_to = d.child_elements_named(r, WSA_NS, "Address").next().map(|x| text_of(&d, x)),
                _ => {}
            }
        } else if ns == WSRM_NS && local == "Sequence" {
            let id = d.child_elements_named(r, WSRM_NS, "Identifier").next().map(|x| text_of(&d, x));
            let n = d.child_elements_named(r, WSRM_NS, "MessageNumber").next().and_then(|x| text_of(&d, x).parse().ok());
            if let (Some(id), Some(n)) = (id, n) {
                a.sequence = Some((id, n));
            }
        }
    }
    a
}

/// Whether a header block is a WS-Addressing or WS-RM header.
pub fn is_wsa_or_rm(header: &str) -> bool {
    match Document::parse(header) {
        Ok(d) => d.document_element().map(|r| d.namespace_uri(r) == WSA_NS || d.namespace_uri(r) == WSRM_NS).unwrap_or(false),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------- RM

pub fn create_sequence_body() -> String {
    format!(
        "<wsrm:CreateSequence xmlns:wsrm=\"{rm}\" xmlns:wsa=\"{wsa}\"><wsrm:AcksTo><wsa:Address>{anon}</wsa:Address></wsrm:AcksTo></wsrm:CreateSequence>",
        rm = WSRM_NS,
        wsa = WSA_NS,
        anon = ANONYMOUS
    )
}

pub fn terminate_sequence_body(id: &str, last: u64) -> String {
    format!("<wsrm:TerminateSequence xmlns:wsrm=\"{}\"><wsrm:Identifier>{}</wsrm:Identifier><wsrm:LastMsgNumber>{}</wsrm:LastMsgNumber></wsrm:TerminateSequence>", WSRM_NS, escape_text(id), last)
}

pub fn sequence_header(id: &str, n: u64) -> String {
    format!("<wsrm:Sequence xmlns:wsrm=\"{}\"><wsrm:Identifier>{}</wsrm:Identifier><wsrm:MessageNumber>{}</wsrm:MessageNumber></wsrm:Sequence>", WSRM_NS, escape_text(id), n)
}

/// The sequence identifier of a `CreateSequenceResponse` body element.
pub fn parse_create_sequence_response(body: &[String]) -> Option<String> {
    let d = Document::parse(body.first()?).ok()?;
    let r = d.document_element()?;
    if d.local_name(r) != "CreateSequenceResponse" {
        return None;
    }
    let id = d.child_elements_named(r, WSRM_NS, "Identifier").next().map(|x| text_of(&d, x));
    id
}

fn ack_header(id: &str, received: &BTreeSet<u64>) -> String {
    let mut ranges = String::new();
    let mut it = received.iter().copied().peekable();
    while let Some(lo) = it.next() {
        let mut hi = lo;
        while it.peek() == Some(&(hi + 1)) {
            hi = it.next().unwrap();
        }
        ranges.push_str(&format!("<wsrm:AcknowledgementRange Lower=\"{}\" Upper=\"{}\"/>", lo, hi));
    }
    format!("<wsrm:SequenceAcknowledgement xmlns:wsrm=\"{}\"><wsrm:Identifier>{}</wsrm:Identifier>{}</wsrm:SequenceAcknowledgement>", WSRM_NS, escape_text(id), ranges)
}

#[derive(Default)]
struct SeqState {
    received: BTreeSet<u64>,
    responses: HashMap<u64, (u16, String)>,
}

/// Server-side RM state (destination sequences).
#[derive(Default)]
pub struct RmDestination {
    seqs: Mutex<HashMap<String, SeqState>>,
}

/// What the service should do with a request.
pub enum RmAction {
    /// A protocol message answered directly (body element, response action).
    Reply(String, &'static str),
    /// A duplicate: resend the cached response.
    Duplicate(u16, String),
    /// Process the message; add the acknowledgement header to the response.
    Process(Option<String>),
    /// Unknown sequence.
    UnknownSequence(String),
}

impl RmDestination {
    /// Classify a request by its action, body and addressing properties.
    pub fn on_request(&self, a: &Addressing, body: &[String]) -> RmAction {
        let action = a.action.as_deref().unwrap_or("");
        let body_local = body.first().and_then(|b| Document::parse(b).ok()).and_then(|d| d.document_element().map(|r| d.local_name(r).to_string())).unwrap_or_default();
        if action == CREATE_SEQUENCE || body_local == "CreateSequence" {
            let id = format!("urn:uuid:{}", cameleer_core::message::generate_id());
            self.seqs.lock().unwrap().insert(id.clone(), SeqState::default());
            return RmAction::Reply(format!("<wsrm:CreateSequenceResponse xmlns:wsrm=\"{}\"><wsrm:Identifier>{}</wsrm:Identifier></wsrm:CreateSequenceResponse>", WSRM_NS, id), CREATE_SEQUENCE_RESPONSE);
        }
        if action == TERMINATE_SEQUENCE || body_local == "TerminateSequence" {
            let id = body.first().and_then(|b| Document::parse(b).ok()).and_then(|d| d.document_element().and_then(|r| d.child_elements_named(r, WSRM_NS, "Identifier").next().map(|x| text_of(&d, x)))).unwrap_or_default();
            self.seqs.lock().unwrap().remove(&id);
            return RmAction::Reply(format!("<wsrm:TerminateSequenceResponse xmlns:wsrm=\"{}\"><wsrm:Identifier>{}</wsrm:Identifier></wsrm:TerminateSequenceResponse>", WSRM_NS, escape_text(&id)), TERMINATE_SEQUENCE_RESPONSE);
        }
        let Some((id, n)) = &a.sequence else { return RmAction::Process(None) };
        let mut seqs = self.seqs.lock().unwrap();
        let Some(st) = seqs.get_mut(id) else { return RmAction::UnknownSequence(id.clone()) };
        if let Some((status, resp)) = st.responses.get(n) {
            return RmAction::Duplicate(*status, resp.clone());
        }
        st.received.insert(*n);
        RmAction::Process(Some(ack_header(id, &st.received)))
    }

    /// Remember the response of a sequence message (for duplicates).
    pub fn store(&self, a: &Addressing, status: u16, response: &str) {
        if let Some((id, n)) = &a.sequence {
            if let Some(st) = self.seqs.lock().unwrap().get_mut(id) {
                st.responses.insert(*n, (status, response.to_string()));
                if st.responses.len() > 1000 {
                    let min = *st.responses.keys().min().unwrap();
                    st.responses.remove(&min);
                }
            }
        }
    }
}

/// Client-side RM state (one source sequence per producer).
#[derive(Default)]
pub struct RmSource {
    pub state: Mutex<Option<(String, u64)>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addressing_and_rm() {
        let h = request_headers("urn:act", "http://x/svc", "urn:uuid:1", None);
        let mut hs = h.clone();
        hs.push(sequence_header("seq-1", 3));
        let a = parse(&hs);
        assert_eq!(a.action.as_deref(), Some("urn:act"));
        assert_eq!(a.reply_to.as_deref(), Some(ANONYMOUS));
        assert_eq!(a.sequence, Some(("seq-1".into(), 3)));
        let r = parse(&reply_headers(&a, "urn:actResponse"));
        assert_eq!(r.relates_to.as_deref(), Some("urn:uuid:1"));
        let dest = RmDestination::default();
        let RmAction::Reply(b, _) = dest.on_request(&Addressing { action: Some(CREATE_SEQUENCE.into()), ..Default::default() }, &[create_sequence_body()]) else { panic!() };
        let id = parse_create_sequence_response(&[b]).unwrap();
        let msg = Addressing { sequence: Some((id.clone(), 1)), ..Default::default() };
        assert!(matches!(dest.on_request(&msg, &[]), RmAction::Process(Some(_))));
        dest.store(&msg, 200, "resp");
        assert!(matches!(dest.on_request(&msg, &[]), RmAction::Duplicate(200, ref s) if s == "resp"));
        let unknown = Addressing { sequence: Some(("nope".into(), 1)), ..Default::default() };
        assert!(matches!(dest.on_request(&unknown, &[]), RmAction::UnknownSequence(_)));
    }
}
