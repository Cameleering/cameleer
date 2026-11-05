//! STOMP 1.2 protocol: frames, client and the `stomp` / `jms` / `activemq` / `amqp` components.
pub mod client;
pub mod component;
pub mod frame;

/// Normalize a destination to its STOMP form: `/queue/x`, `/topic/x`, `/temp-queue/x`,
/// `/temp-topic/x`. Accepts `queue://x`, `queue:x`, `topic://x`, `topic:x`,
/// `temp-queue://x`, `temp-topic:x` and bare names (queues).
pub fn normalize_destination(d: &str) -> String {
    let d = d.trim();
    if d.starts_with('/') {
        return d.to_string();
    }
    for kind in ["temp-queue", "temp-topic", "queue", "topic"] {
        for sep in ["://", ":"] {
            if let Some(rest) = d.strip_prefix(&format!("{}{}", kind, sep)) {
                return format!("/{}/{}", kind, rest);
            }
        }
    }
    format!("/queue/{}", d)
}

/// JMS style name of a STOMP destination: `/queue/x` becomes `queue://x`.
pub fn jms_destination_name(d: &str) -> String {
    for kind in ["temp-queue", "temp-topic", "queue", "topic"] {
        if let Some(rest) = d.strip_prefix(&format!("/{}/", kind)) {
            return format!("{}://{}", kind, rest);
        }
    }
    d.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations() {
        assert_eq!(normalize_destination("foo"), "/queue/foo");
        assert_eq!(normalize_destination("queue://foo"), "/queue/foo");
        assert_eq!(normalize_destination("topic:bar"), "/topic/bar");
        assert_eq!(normalize_destination("temp-queue://x"), "/temp-queue/x");
        assert_eq!(normalize_destination("/topic/a"), "/topic/a");
        assert_eq!(jms_destination_name("/temp-queue/x"), "temp-queue://x");
        assert_eq!(jms_destination_name("/queue/a.b"), "queue://a.b");
    }
}
