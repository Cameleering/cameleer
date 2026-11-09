//! Associations (PS3.8 section 7): requesting and accepting, sending and receiving DIMSE
//! messages split into PDVs, release and abort.

use std::collections::VecDeque;
use std::io::BufReader;
use std::time::Duration;

use cameleer_core::error::{CamelError, Result};
use cameleer_tls::camel_support::EndpointTls;
use cameleer_tls::net::NetStream;

use crate::codec::{self, ReadOptions};
use crate::object::DataSet;
use crate::ts::{self, Encoding};
use crate::ul::{self, pc_result, AssociateAc, AssociateRj, AssociateRq, Pdu, Pdv, DICOM_ASSOCIATION_EXCEPTION};

/// The maximum PDU length this side announces by default (DCMTK's default).
pub const DEFAULT_MAX_PDU: u32 = 16384;

fn aerr(msg: impl Into<String>) -> CamelError {
    CamelError::new(DICOM_ASSOCIATION_EXCEPTION, msg)
}

/// A received DIMSE message.
#[derive(Clone, Debug)]
pub struct Message {
    pub pc_id: u8,
    pub command: DataSet,
    /// The data set bytes, encoded in the transfer syntax of the presentation context.
    pub data: Option<Vec<u8>>,
}

/// What [`Association::receive`] returned.
#[derive(Debug)]
pub enum Received {
    Message(Message),
    /// The peer asked to release; the reply has been sent and the connection closed.
    Released,
    /// The peer aborted, or closed the connection.
    Aborted(String),
}

/// An established association.
pub struct Association {
    stream: NetStream,
    reader: BufReader<NetStream>,
    pub rq: AssociateRq,
    pub ac: AssociateAc,
    /// Whether this side requested the association.
    pub requestor: bool,
    peer_max_pdu: u32,
    local_max_pdu: u32,
    pending: VecDeque<Pdv>,
    next_id: u16,
    pub remote: String,
    closed: bool,
}

impl Association {
    /// Connect and request an association; fails with `DicomAssociationException` when it is
    /// rejected (the message says why).
    pub fn request(host: &str, port: u16, tls: Option<&EndpointTls>, rq: AssociateRq, connect_timeout: Duration, timeout: Duration) -> Result<Association> {
        let what = format!("{}:{}", host, port);
        let stream = match tls {
            Some(t) => t.connect(host, port, connect_timeout)?,
            None => {
                let tcp = cameleer_tls::net::connect_tcp(host, port, connect_timeout).map_err(|e| match e.kind() {
                    std::io::ErrorKind::ConnectionRefused => CamelError::new(cameleer_core::error::kinds::CONNECT, format!("Connection refused: {}", what)),
                    _ => CamelError::io(format!("{}: {}", e, what)),
                })?;
                let _ = tcp.set_nodelay(true);
                NetStream::plain(tcp)
            }
        };
        let to = if timeout.is_zero() { None } else { Some(timeout) };
        let _ = stream.set_read_timeout(to);
        let reader = BufReader::with_capacity(64 * 1024, stream.try_clone().map_err(|e| CamelError::io(e.to_string()))?);
        let mut a = Association {
            stream,
            reader,
            local_max_pdu: rq.user.max_pdu_length,
            ac: AssociateAc { called_ae: String::new(), calling_ae: String::new(), application_context: String::new(), presentation_contexts: Vec::new(), user: Default::default() },
            rq,
            requestor: true,
            peer_max_pdu: 0,
            pending: VecDeque::new(),
            next_id: 1,
            remote: what.clone(),
            closed: false,
        };
        ul::write_pdu(&mut &a.stream, &Pdu::AssociateRq(a.rq.clone()))?;
        match ul::read_pdu(&mut a.reader, 0)? {
            Some(Pdu::AssociateAc(ac)) => {
                a.peer_max_pdu = ac.user.max_pdu_length;
                a.ac = ac;
                Ok(a)
            }
            Some(Pdu::AssociateRj(rj)) => {
                a.close();
                Err(aerr(format!("Association with {} ({}) rejected: {}", a.rq.called_ae, what, rj.describe())))
            }
            Some(Pdu::Abort { source, reason }) => {
                a.close();
                Err(aerr(format!("Association with {} aborted during negotiation (source {}, reason {})", what, source, reason)))
            }
            Some(other) => {
                a.abort();
                Err(aerr(format!("Unexpected {:?} from {} during association negotiation", pdu_name(&other), what)))
            }
            None => Err(aerr(format!("{} closed the connection during association negotiation", what))),
        }
    }

    /// Accept (or reject) an association on a connected stream. `decide` answers the request;
    /// returning `Err` sends an A-ASSOCIATE-RJ. `Ok(None)` when the peer went away or was
    /// rejected.
    pub fn accept(stream: NetStream, local_max_pdu: u32, timeout: Duration, decide: impl FnOnce(&AssociateRq) -> std::result::Result<AssociateAc, AssociateRj>) -> Result<Option<Association>> {
        let remote = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
        let to = if timeout.is_zero() { None } else { Some(timeout) };
        let _ = stream.set_read_timeout(to);
        let mut reader = BufReader::with_capacity(64 * 1024, stream.try_clone().map_err(|e| CamelError::io(e.to_string()))?);
        let rq = match ul::read_pdu(&mut reader, 0)? {
            Some(Pdu::AssociateRq(rq)) => rq,
            Some(_) => {
                let _ = ul::write_pdu(&mut &stream, &Pdu::Abort { source: 2, reason: 2 });
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return Err(aerr(format!("{} sent something other than an A-ASSOCIATE-RQ", remote)));
            }
            None => return Ok(None),
        };
        match decide(&rq) {
            Ok(ac) => {
                ul::write_pdu(&mut &stream, &Pdu::AssociateAc(ac.clone()))?;
                Ok(Some(Association { peer_max_pdu: rq.user.max_pdu_length, stream, reader, rq, ac, requestor: false, local_max_pdu, pending: VecDeque::new(), next_id: 1, remote, closed: false }))
            }
            Err(rj) => {
                let _ = ul::write_pdu(&mut &stream, &Pdu::AssociateRj(rj));
                let _ = stream.shutdown(std::net::Shutdown::Both);
                Ok(None)
            }
        }
    }

    /// The calling AE title.
    pub fn calling_ae(&self) -> &str {
        &self.rq.calling_ae
    }

    /// The called AE title.
    pub fn called_ae(&self) -> &str {
        &self.rq.called_ae
    }

    /// A new Message ID.
    pub fn next_message_id(&mut self) -> u16 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    /// The abstract syntax of a presentation context.
    pub fn abstract_syntax(&self, pc_id: u8) -> Option<&str> {
        self.rq.presentation_contexts.iter().find(|p| p.id == pc_id).map(|p| p.abstract_syntax.as_str())
    }

    /// The accepted transfer syntax of a presentation context.
    pub fn transfer_syntax(&self, pc_id: u8) -> Option<&str> {
        self.ac.presentation_contexts.iter().find(|p| p.id == pc_id && p.result == pc_result::ACCEPTANCE).map(|p| p.transfer_syntax.as_str())
    }

    /// The accepted presentation contexts for an abstract syntax as (id, transfer syntax),
    /// those with `preferred` first.
    pub fn contexts_for(&self, abstract_syntax: &str, preferred: Option<&str>) -> Vec<(u8, String)> {
        let mut v: Vec<(u8, String)> = self
            .rq
            .presentation_contexts
            .iter()
            .filter(|p| p.abstract_syntax == abstract_syntax)
            .filter_map(|p| self.transfer_syntax(p.id).map(|t| (p.id, t.to_string())))
            .collect();
        if let Some(pref) = preferred {
            v.sort_by_key(|(_, t)| t != pref);
        }
        v
    }

    fn max_fragment(&self) -> usize {
        let m = if self.peer_max_pdu == 0 { 1 << 20 } else { self.peer_max_pdu as usize };
        // PDV fragments of even length (DCMTK rejects odd ones)
        m.saturating_sub(6).max(64) & !1
    }

    /// Send a message: the command set (implicit VR little endian) and optionally the data set
    /// bytes (already encoded in the context's transfer syntax), split to the peer's maximum
    /// PDU length.
    pub fn send(&mut self, pc_id: u8, command: &DataSet, data: Option<&[u8]>) -> Result<()> {
        let mut cmd = Vec::new();
        codec::write_dataset(&mut cmd, command, Encoding::IMPLICIT_LE)?;
        self.send_fragments(pc_id, true, &cmd)?;
        if let Some(d) = data {
            self.send_fragments(pc_id, false, d)?;
        }
        Ok(())
    }

    fn send_fragments(&mut self, pc_id: u8, command: bool, bytes: &[u8]) -> Result<()> {
        let max = self.max_fragment();
        if bytes.is_empty() {
            return ul::write_pdu(&mut &self.stream, &Pdu::PData(vec![Pdv { pc_id, command, last: true, data: Vec::new() }]));
        }
        let mut chunks = bytes.chunks(max).peekable();
        while let Some(c) = chunks.next() {
            let last = chunks.peek().is_none();
            ul::write_pdu(&mut &self.stream, &Pdu::PData(vec![Pdv { pc_id, command, last, data: c.to_vec() }]))?;
        }
        Ok(())
    }

    fn next_pdv(&mut self) -> Result<std::result::Result<Pdv, Received>> {
        loop {
            if let Some(p) = self.pending.pop_front() {
                return Ok(Ok(p));
            }
            match ul::read_pdu(&mut self.reader, self.local_max_pdu)? {
                Some(Pdu::PData(pdvs)) => self.pending.extend(pdvs),
                Some(Pdu::ReleaseRq) => {
                    let _ = ul::write_pdu(&mut &self.stream, &Pdu::ReleaseRp);
                    self.close();
                    return Ok(Err(Received::Released));
                }
                Some(Pdu::Abort { source, reason }) => {
                    self.close();
                    return Ok(Err(Received::Aborted(format!("A-ABORT from {} (source {}, reason {})", self.remote, source, reason))));
                }
                Some(other) => {
                    self.abort();
                    return Err(aerr(format!("Unexpected {} from {} on an established association", pdu_name(&other), self.remote)));
                }
                None => {
                    self.closed = true;
                    return Ok(Err(Received::Aborted(format!("{} closed the connection", self.remote))));
                }
            }
        }
    }

    /// Receive the next message (command plus data set when the command announces one).
    pub fn receive(&mut self) -> Result<Received> {
        let mut cmd = Vec::new();
        let pc;
        loop {
            let p = match self.next_pdv()? {
                Ok(p) => p,
                Err(r) => return Ok(r),
            };
            if !p.command {
                self.abort();
                return Err(aerr("Data set fragment received before its command"));
            }
            cmd.extend_from_slice(&p.data);
            if p.last {
                pc = p.pc_id;
                break;
            }
        }
        let command = codec::read_dataset(&cmd, Encoding::IMPLICIT_LE, ReadOptions::default())?;
        let data = if crate::dimse::has_data_set(&command) {
            let mut d = Vec::new();
            loop {
                let p = match self.next_pdv()? {
                    Ok(p) => p,
                    Err(r) => return Ok(r),
                };
                if p.command {
                    self.abort();
                    return Err(aerr("Command fragment received inside a data set"));
                }
                d.extend_from_slice(&p.data);
                if p.last {
                    break;
                }
            }
            Some(d)
        } else {
            None
        };
        Ok(Received::Message(Message { pc_id: pc, command, data }))
    }

    /// Receive a message, failing on release or abort (for a requestor waiting for a response).
    pub fn receive_message(&mut self) -> Result<Message> {
        match self.receive()? {
            Received::Message(m) => Ok(m),
            Received::Released => Err(aerr(format!("{} released the association while a response was expected", self.remote))),
            Received::Aborted(why) => Err(aerr(why)),
        }
    }

    /// Release the association (A-RELEASE-RQ, wait for the reply) and close the connection.
    pub fn release(mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        ul::write_pdu(&mut &self.stream, &Pdu::ReleaseRq)?;
        // messages still in flight are dropped; a release collision is answered
        for _ in 0..64 {
            match ul::read_pdu(&mut self.reader, self.local_max_pdu) {
                Ok(Some(Pdu::ReleaseRp)) | Ok(None) | Ok(Some(Pdu::Abort { .. })) => break,
                Ok(Some(Pdu::ReleaseRq)) => {
                    let _ = ul::write_pdu(&mut &self.stream, &Pdu::ReleaseRp);
                }
                Ok(Some(_)) => continue,
                Err(_) => break,
            }
        }
        self.close();
        Ok(())
    }

    /// Abort the association (A-ABORT from the service user) and close the connection.
    pub fn abort(&mut self) {
        if !self.closed {
            let _ = ul::write_pdu(&mut &self.stream, &Pdu::Abort { source: 0, reason: 0 });
            self.close();
        }
    }

    fn close(&mut self) {
        self.closed = true;
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }

    /// Whether the connection is closed.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// The underlying stream (peer certificates of a TLS association).
    pub fn stream(&self) -> &NetStream {
        &self.stream
    }

    /// Set the read timeout for the next responses.
    pub fn set_timeout(&self, timeout: Duration) {
        let _ = self.stream.set_read_timeout(if timeout.is_zero() { None } else { Some(timeout) });
    }
}

impl Drop for Association {
    fn drop(&mut self) {
        if !self.closed {
            self.abort();
        }
    }
}

fn pdu_name(p: &Pdu) -> &'static str {
    match p {
        Pdu::AssociateRq(_) => "A-ASSOCIATE-RQ",
        Pdu::AssociateAc(_) => "A-ASSOCIATE-AC",
        Pdu::AssociateRj(_) => "A-ASSOCIATE-RJ",
        Pdu::PData(_) => "P-DATA-TF",
        Pdu::ReleaseRq => "A-RELEASE-RQ",
        Pdu::ReleaseRp => "A-RELEASE-RP",
        Pdu::Abort { .. } => "A-ABORT",
    }
}

/// Native transfer syntaxes in the order this side prefers them.
pub const NATIVE_TRANSFER_SYNTAXES: [&str; 4] = [ts::EXPLICIT_VR_LITTLE_ENDIAN, ts::IMPLICIT_VR_LITTLE_ENDIAN, ts::DEFLATED_EXPLICIT_VR_LITTLE_ENDIAN, ts::EXPLICIT_VR_BIG_ENDIAN];
