//! Incremental Server-Sent Events decoder (`text/event-stream`): the response body is streamed
//! into it (`HttpClient::send_to`) and each event (`event:` / `data:` / `id:` fields, a blank
//! line dispatching it, `:` comments ignored, multi-line `data` joined with `\n`) is handed to a
//! callback as soon as it is complete. Used by the Azure OpenAI and Vertex AI streaming
//! operations.

use std::io;

use camel_core::error::Result;

/// One server-sent event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
}

/// The decoder (an `io::Write` sink).
pub struct SseDecoder<F: FnMut(SseEvent) -> Result<()>> {
    buf: Vec<u8>,
    current: SseEvent,
    has_data: bool,
    on_event: F,
    error: Option<camel_core::error::CamelError>,
    events: usize,
}

impl<F: FnMut(SseEvent) -> Result<()>> SseDecoder<F> {
    pub fn new(on_event: F) -> Self {
        SseDecoder { buf: Vec::new(), current: SseEvent::default(), has_data: false, on_event, error: None, events: 0 }
    }

    /// Number of events dispatched.
    pub fn events(&self) -> usize {
        self.events
    }

    /// The callback error that aborted the stream, if any.
    pub fn take_error(&mut self) -> Option<camel_core::error::CamelError> {
        self.error.take()
    }

    fn line(&mut self, line: &str) -> Result<()> {
        if line.is_empty() {
            if self.has_data || self.current.event.is_some() {
                let ev = std::mem::take(&mut self.current);
                self.has_data = false;
                self.events += 1;
                (self.on_event)(ev)?;
            }
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.current.event = Some(value.to_string()),
            "data" => {
                if self.has_data {
                    self.current.data.push('\n');
                }
                self.current.data.push_str(value);
                self.has_data = true;
            }
            "id" => self.current.id = Some(value.to_string()),
            _ => {}
        }
        Ok(())
    }

    /// Feed bytes.
    pub fn push(&mut self, data: &[u8]) -> Result<()> {
        self.buf.extend_from_slice(data);
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let text = String::from_utf8_lossy(&line).to_string();
            self.line(&text)?;
        }
        Ok(())
    }

    /// Dispatch a final event not followed by a blank line.
    pub fn finish(&mut self) -> Result<()> {
        if !self.buf.is_empty() {
            let rest = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).to_string();
            self.line(rest.trim_end_matches('\r'))?;
        }
        self.line("")
    }
}

impl<F: FnMut(SseEvent) -> Result<()>> io::Write for SseDecoder<F> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.error.is_some() {
            return Err(io::Error::other("event stream aborted"));
        }
        if let Err(e) = self.push(data) {
            let msg = e.to_string();
            self.error = Some(e);
            return Err(io::Error::other(msg));
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Encode events as an SSE body (emulators).
pub fn encode(events: &[(Option<&str>, String)]) -> Vec<u8> {
    let mut out = String::new();
    for (ev, data) in events {
        if let Some(e) = ev {
            out.push_str(&format!("event: {}\n", e));
        }
        for l in data.split('\n') {
            out.push_str(&format!("data: {}\n", l));
        }
        out.push('\n');
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_split_events() {
        let mut got = Vec::new();
        {
            let mut d = SseDecoder::new(|e| {
                got.push(e);
                Ok(())
            });
            let body = b": comment\r\nevent: delta\r\ndata: {\"a\":\r\ndata: 1}\r\n\r\ndata: [DONE]\n\ndata: tail";
            for chunk in body.chunks(5) {
                d.push(chunk).unwrap();
            }
            d.finish().unwrap();
        }
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], SseEvent { event: Some("delta".into()), data: "{\"a\":\n1}".into(), id: None });
        assert_eq!(got[1].data, "[DONE]");
        assert_eq!(got[2].data, "tail");
    }
}
