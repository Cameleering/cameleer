//! Request/response over AMQP management nodes and claims-based security (CBS):
//! `put-token` of SAS tokens on the `$cbs` node (Azure Service Bus / Event Hubs).

use std::time::Duration;

use camel_core::error::Result;

use crate::client::{LinkOptions, Receiver, Sender, Session};
use crate::codec::AmqpValue;
use crate::message::{id_to_string, Message};
use crate::util::{amqp_failure, register_kinds, unique_id};

/// Token type of Azure SAS tokens.
pub const SAS_TOKEN_TYPE: &str = "servicebus.windows.net:sastoken";
/// Token type of JWT (Azure AD) tokens.
pub const JWT_TOKEN_TYPE: &str = "jwt";

/// A request/response client on a node (`$cbs`, `$management`, `<entity>/$management`):
/// a sender to the node and a receiver from it whose target address is the reply-to.
pub struct RequestResponse {
    sender: Sender,
    receiver: Receiver,
    reply_to: String,
    timeout: Duration,
}

impl RequestResponse {
    /// Attach the two links to `node` on `session`.
    pub fn open(session: &Session, node: &str, timeout: Duration) -> Result<RequestResponse> {
        let reply_to = format!("{}-reply-{}", node.trim_start_matches('$').replace('/', "-"), unique_id());
        let sender = session.sender(LinkOptions::new(node).name(&format!("{}-sender-{}", node, unique_id())))?;
        let receiver = session.receiver(LinkOptions::new(node).name(&format!("{}-receiver-{}", node, unique_id())).local_address(&reply_to).credit(10))?;
        Ok(RequestResponse { sender, receiver, reply_to, timeout })
    }

    /// Send a request (message-id and reply-to are set) and wait for the response with
    /// the matching correlation-id.
    pub fn request(&self, mut msg: Message) -> Result<Message> {
        let id = unique_id();
        {
            let p = msg.props_mut();
            p.message_id = Some(AmqpValue::String(id.clone()));
            p.reply_to = Some(self.reply_to.clone());
        }
        self.sender.send_accepted(&msg, self.timeout)?;
        let deadline = std::time::Instant::now() + self.timeout;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let Some(d) = self.receiver.receive(left)? else {
                return Err(camel_core::error::CamelError::new(camel_core::error::kinds::TIMEOUT, "Timed out waiting for the management response"));
            };
            let _ = self.receiver.accept(&d);
            let corr = d.message.properties.as_ref().and_then(|p| p.correlation_id.as_ref()).map(id_to_string);
            if corr.as_deref() == Some(id.as_str()) || corr.is_none() {
                return Ok(d.message);
            }
        }
    }

    /// The status code of a response (`status-code` or `statusCode`).
    pub fn status(response: &Message) -> (i64, String) {
        let code = response.app_property("status-code").or_else(|| response.app_property("statusCode")).and_then(|v| v.as_i64()).unwrap_or(0);
        let desc = response
            .app_property("status-description")
            .or_else(|| response.app_property("statusDescription"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        (code, desc)
    }

    pub fn close(&self) {
        self.sender.close();
        self.receiver.close();
    }
}

/// Put a token on the `$cbs` node for `audience` (e.g. `sb://ns.servicebus.windows.net/queue`).
pub fn put_token(session: &Session, audience: &str, token: &str, token_type: &str, timeout: Duration) -> Result<()> {
    register_kinds();
    let rr = RequestResponse::open(session, "$cbs", timeout)?;
    let mut msg = Message::text(token);
    msg.set_app_property("operation", AmqpValue::str("put-token"));
    msg.set_app_property("type", AmqpValue::str(token_type));
    msg.set_app_property("name", AmqpValue::str(audience));
    let r = rr.request(msg);
    rr.close();
    let resp = r?;
    let (code, desc) = RequestResponse::status(&resp);
    if code == 200 || code == 202 {
        return Ok(());
    }
    let kind = if code == 401 || code == 403 { crate::util::kinds::UNAUTHORIZED } else { crate::util::kinds::AMQP };
    if code == 0 {
        return Err(amqp_failure("CBS put-token: response without status code"));
    }
    Err(camel_core::error::CamelError::new(kind, format!("CBS put-token for {} failed: {} {}", audience, code, desc)))
}
