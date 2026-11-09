//! Upper Layer protocol data units (PS3.8 section 9.3): association negotiation, P-DATA,
//! release and abort. All fields are big endian.

use std::io::{Read, Write};

use cameleer_core::error::{CamelError, Result};

/// The DICOM Application Context Name.
pub const APPLICATION_CONTEXT: &str = "1.2.840.10008.3.1.1.1";

/// Exception kind for Upper Layer protocol errors.
pub const DICOM_ASSOCIATION_EXCEPTION: &str = "DicomAssociationException";

fn aerr(msg: impl Into<String>) -> CamelError {
    CamelError::new(DICOM_ASSOCIATION_EXCEPTION, msg)
}

/// A presentation context proposed in an A-ASSOCIATE-RQ.
#[derive(Clone, Debug, PartialEq)]
pub struct PresentationContextRq {
    /// Odd number 1-255.
    pub id: u8,
    pub abstract_syntax: String,
    pub transfer_syntaxes: Vec<String>,
}

/// Result of a presentation context in an A-ASSOCIATE-AC.
pub mod pc_result {
    pub const ACCEPTANCE: u8 = 0;
    pub const USER_REJECTION: u8 = 1;
    pub const NO_REASON: u8 = 2;
    pub const ABSTRACT_SYNTAX_NOT_SUPPORTED: u8 = 3;
    pub const TRANSFER_SYNTAXES_NOT_SUPPORTED: u8 = 4;
}

/// A presentation context answered in an A-ASSOCIATE-AC.
#[derive(Clone, Debug, PartialEq)]
pub struct PresentationContextAc {
    pub id: u8,
    pub result: u8,
    pub transfer_syntax: String,
}

/// SCP/SCU Role Selection (PS3.7 D.3.3.4).
#[derive(Clone, Debug, PartialEq)]
pub struct RoleSelection {
    pub sop_class: String,
    pub scu: bool,
    pub scp: bool,
}

/// User Identity Negotiation request (PS3.7 D.3.3.7): type 1 username, 2 username and
/// passcode, 3 Kerberos, 4 SAML, 5 JWT.
#[derive(Clone, Debug, PartialEq)]
pub struct UserIdentity {
    pub kind: u8,
    pub positive_response_requested: bool,
    pub primary: Vec<u8>,
    pub secondary: Vec<u8>,
}

/// The User Information item.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct UserInfo {
    /// Maximum length of P-DATA-TF PDUs this side receives (0 = unlimited).
    pub max_pdu_length: u32,
    pub implementation_class_uid: String,
    pub implementation_version_name: Option<String>,
    /// Asynchronous Operations Window (invoked, performed).
    pub async_ops: Option<(u16, u16)>,
    pub role_selections: Vec<RoleSelection>,
    /// SOP Class Extended Negotiation (SOP class, application information).
    pub extended: Vec<(String, Vec<u8>)>,
    pub user_identity: Option<UserIdentity>,
    /// The server response of a User Identity Negotiation (A-ASSOCIATE-AC).
    pub user_identity_response: Option<Vec<u8>>,
}

/// A-ASSOCIATE-RQ.
#[derive(Clone, Debug, PartialEq)]
pub struct AssociateRq {
    pub called_ae: String,
    pub calling_ae: String,
    pub application_context: String,
    pub presentation_contexts: Vec<PresentationContextRq>,
    pub user: UserInfo,
}

/// A-ASSOCIATE-AC.
#[derive(Clone, Debug, PartialEq)]
pub struct AssociateAc {
    pub called_ae: String,
    pub calling_ae: String,
    pub application_context: String,
    pub presentation_contexts: Vec<PresentationContextAc>,
    pub user: UserInfo,
}

/// A-ASSOCIATE-RJ result, source and reason (PS3.8 Table 9-21).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AssociateRj {
    /// 1 rejected-permanent, 2 rejected-transient.
    pub result: u8,
    /// 1 service user, 2 service provider (ACSE), 3 service provider (presentation).
    pub source: u8,
    /// For source 1: 1 no reason, 2 application context not supported, 3 calling AE title not
    /// recognized, 7 called AE title not recognized.
    pub reason: u8,
}

impl AssociateRj {
    /// A readable description.
    pub fn describe(&self) -> String {
        let reason = match (self.source, self.reason) {
            (1, 1) => "no reason given",
            (1, 2) => "application context name not supported",
            (1, 3) => "calling AE title not recognized",
            (1, 7) => "called AE title not recognized",
            (2, 1) => "no reason given (ACSE)",
            (2, 2) => "protocol version not supported",
            (3, 1) => "temporary congestion",
            (3, 2) => "local limit exceeded",
            _ => "unknown reason",
        };
        format!("{} ({}, source {}, reason {})", reason, if self.result == 1 { "permanent" } else { "transient" }, self.source, self.reason)
    }
}

/// One Presentation Data Value of a P-DATA-TF.
#[derive(Clone, Debug, PartialEq)]
pub struct Pdv {
    pub pc_id: u8,
    /// Command (true) or data set (false) fragment.
    pub command: bool,
    /// Last fragment of the command or data set.
    pub last: bool,
    pub data: Vec<u8>,
}

/// A protocol data unit.
#[derive(Clone, Debug, PartialEq)]
pub enum Pdu {
    AssociateRq(AssociateRq),
    AssociateAc(AssociateAc),
    AssociateRj(AssociateRj),
    PData(Vec<Pdv>),
    ReleaseRq,
    ReleaseRp,
    /// A-ABORT: source (0 service user, 2 service provider) and reason.
    Abort { source: u8, reason: u8 },
}

fn ae_field(s: &str) -> [u8; 16] {
    let mut f = [b' '; 16];
    for (i, b) in s.bytes().take(16).enumerate() {
        f[i] = b;
    }
    f
}

fn ae_string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).trim().to_string()
}

fn uid_string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).trim_end_matches(['\0', ' ']).to_string()
}

fn put_item(out: &mut Vec<u8>, kind: u8, body: &[u8]) {
    out.push(kind);
    out.push(0);
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(body);
}

fn user_info_bytes(u: &UserInfo) -> Vec<u8> {
    let mut b = Vec::new();
    put_item(&mut b, 0x51, &u.max_pdu_length.to_be_bytes());
    put_item(&mut b, 0x52, u.implementation_class_uid.as_bytes());
    if let Some((i, p)) = u.async_ops {
        let mut v = i.to_be_bytes().to_vec();
        v.extend_from_slice(&p.to_be_bytes());
        put_item(&mut b, 0x53, &v);
    }
    for r in &u.role_selections {
        let mut v = (r.sop_class.len() as u16).to_be_bytes().to_vec();
        v.extend_from_slice(r.sop_class.as_bytes());
        v.push(r.scu as u8);
        v.push(r.scp as u8);
        put_item(&mut b, 0x54, &v);
    }
    if let Some(n) = &u.implementation_version_name {
        put_item(&mut b, 0x55, n.as_bytes());
    }
    for (uid, info) in &u.extended {
        let mut v = (uid.len() as u16).to_be_bytes().to_vec();
        v.extend_from_slice(uid.as_bytes());
        v.extend_from_slice(info);
        put_item(&mut b, 0x56, &v);
    }
    if let Some(id) = &u.user_identity {
        let mut v = vec![id.kind, id.positive_response_requested as u8];
        v.extend_from_slice(&(id.primary.len() as u16).to_be_bytes());
        v.extend_from_slice(&id.primary);
        v.extend_from_slice(&(id.secondary.len() as u16).to_be_bytes());
        v.extend_from_slice(&id.secondary);
        put_item(&mut b, 0x58, &v);
    }
    if let Some(r) = &u.user_identity_response {
        let mut v = (r.len() as u16).to_be_bytes().to_vec();
        v.extend_from_slice(r);
        put_item(&mut b, 0x59, &v);
    }
    b
}

fn assoc_header(out: &mut Vec<u8>, called: &str, calling: &str) {
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&ae_field(called));
    out.extend_from_slice(&ae_field(calling));
    out.extend_from_slice(&[0; 32]);
}

/// Encode a PDU.
pub fn encode(pdu: &Pdu) -> Vec<u8> {
    let (kind, body) = match pdu {
        Pdu::AssociateRq(rq) => {
            let mut b = Vec::new();
            assoc_header(&mut b, &rq.called_ae, &rq.calling_ae);
            put_item(&mut b, 0x10, rq.application_context.as_bytes());
            for pc in &rq.presentation_contexts {
                let mut v = vec![pc.id, 0, 0, 0];
                put_item(&mut v, 0x30, pc.abstract_syntax.as_bytes());
                for ts in &pc.transfer_syntaxes {
                    put_item(&mut v, 0x40, ts.as_bytes());
                }
                put_item(&mut b, 0x20, &v);
            }
            put_item(&mut b, 0x50, &user_info_bytes(&rq.user));
            (0x01, b)
        }
        Pdu::AssociateAc(ac) => {
            let mut b = Vec::new();
            assoc_header(&mut b, &ac.called_ae, &ac.calling_ae);
            put_item(&mut b, 0x10, ac.application_context.as_bytes());
            for pc in &ac.presentation_contexts {
                let mut v = vec![pc.id, 0, pc.result, 0];
                put_item(&mut v, 0x40, pc.transfer_syntax.as_bytes());
                put_item(&mut b, 0x21, &v);
            }
            put_item(&mut b, 0x50, &user_info_bytes(&ac.user));
            (0x02, b)
        }
        Pdu::AssociateRj(rj) => (0x03, vec![0, rj.result, rj.source, rj.reason]),
        Pdu::PData(pdvs) => {
            let mut b = Vec::new();
            for p in pdvs {
                b.extend_from_slice(&((p.data.len() + 2) as u32).to_be_bytes());
                b.push(p.pc_id);
                b.push((p.command as u8) | ((p.last as u8) << 1));
                b.extend_from_slice(&p.data);
            }
            (0x04, b)
        }
        Pdu::ReleaseRq => (0x05, vec![0; 4]),
        Pdu::ReleaseRp => (0x06, vec![0; 4]),
        Pdu::Abort { source, reason } => (0x07, vec![0, 0, *source, *reason]),
    };
    let mut out = Vec::with_capacity(body.len() + 6);
    out.push(kind);
    out.push(0);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// Write a PDU.
pub fn write_pdu<W: Write>(w: &mut W, pdu: &Pdu) -> Result<()> {
    w.write_all(&encode(pdu)).and_then(|_| w.flush()).map_err(|e| CamelError::io(format!("Cannot send a DICOM PDU: {}", e)))
}

/// Read a PDU; `max_len` bounds the PDU length accepted (0 = 64 MiB). `Ok(None)` when the
/// peer closed the connection before a PDU started.
pub fn read_pdu<R: Read>(r: &mut R, max_len: u32) -> Result<Option<Pdu>> {
    let mut head = [0u8; 6];
    let mut got = 0;
    while got < 6 {
        match r.read(&mut head[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(aerr("Connection closed inside a PDU header")),
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Err(CamelError::new("SocketTimeoutException", "Timed out waiting for a DICOM PDU"));
            }
            Err(e) => return Err(CamelError::io(format!("Cannot read a DICOM PDU: {}", e))),
        }
    }
    let kind = head[0];
    let len = u32::from_be_bytes([head[2], head[3], head[4], head[5]]);
    let limit = if max_len == 0 { 64 << 20 } else { max_len.saturating_add(6).max(1 << 16) };
    if len > limit {
        return Err(aerr(format!("PDU of {} bytes exceeds the limit of {}", len, limit)));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).map_err(|e| aerr(format!("Connection closed inside a PDU: {}", e)))?;
    decode(kind, &body).map(Some)
}

struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.p + n > self.b.len() {
            return Err(aerr("Truncated PDU"));
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn done(&self) -> bool {
        self.p >= self.b.len()
    }
    /// An item: type, reserved, u16 length, body.
    fn item(&mut self) -> Result<(u8, &'a [u8])> {
        let t = self.u8()?;
        self.u8()?;
        let l = self.u16()? as usize;
        Ok((t, self.take(l)?))
    }
}

fn decode_user_info(b: &[u8]) -> Result<UserInfo> {
    let mut u = UserInfo::default();
    let mut c = Cur { b, p: 0 };
    while !c.done() {
        let (t, v) = c.item()?;
        let mut s = Cur { b: v, p: 0 };
        match t {
            0x51 => u.max_pdu_length = s.u32()?,
            0x52 => u.implementation_class_uid = uid_string(v),
            0x53 => u.async_ops = Some((s.u16()?, s.u16()?)),
            0x54 => {
                let l = s.u16()? as usize;
                let uid = uid_string(s.take(l)?);
                u.role_selections.push(RoleSelection { sop_class: uid, scu: s.u8()? != 0, scp: s.u8()? != 0 });
            }
            0x55 => u.implementation_version_name = Some(ae_string(v)),
            0x56 => {
                let l = s.u16()? as usize;
                let uid = uid_string(s.take(l)?);
                u.extended.push((uid, v[s.p..].to_vec()));
            }
            0x58 => {
                let kind = s.u8()?;
                let pos = s.u8()? != 0;
                let l = s.u16()? as usize;
                let primary = s.take(l)?.to_vec();
                let l2 = s.u16()? as usize;
                let secondary = s.take(l2)?.to_vec();
                u.user_identity = Some(UserIdentity { kind, positive_response_requested: pos, primary, secondary });
            }
            0x59 => {
                let l = s.u16()? as usize;
                u.user_identity_response = Some(s.take(l)?.to_vec());
            }
            _ => {} // 0x57 SOP class common extended and unknown items are ignored
        }
    }
    Ok(u)
}

/// Decode a PDU body.
pub fn decode(kind: u8, body: &[u8]) -> Result<Pdu> {
    let mut c = Cur { b: body, p: 0 };
    match kind {
        0x01 | 0x02 => {
            let _version = c.u16()?;
            c.take(2)?;
            let called = ae_string(c.take(16)?);
            let calling = ae_string(c.take(16)?);
            c.take(32)?;
            let mut app = String::new();
            let mut pcs_rq = Vec::new();
            let mut pcs_ac = Vec::new();
            let mut user = UserInfo::default();
            while !c.done() {
                let (t, v) = c.item()?;
                match t {
                    0x10 => app = uid_string(v),
                    0x20 => {
                        let mut s = Cur { b: v, p: 0 };
                        let id = s.u8()?;
                        s.take(3)?;
                        let mut pc = PresentationContextRq { id, abstract_syntax: String::new(), transfer_syntaxes: Vec::new() };
                        while !s.done() {
                            let (st, sv) = s.item()?;
                            match st {
                                0x30 => pc.abstract_syntax = uid_string(sv),
                                0x40 => pc.transfer_syntaxes.push(uid_string(sv)),
                                _ => {}
                            }
                        }
                        pcs_rq.push(pc);
                    }
                    0x21 => {
                        let mut s = Cur { b: v, p: 0 };
                        let id = s.u8()?;
                        s.u8()?;
                        let result = s.u8()?;
                        s.u8()?;
                        let mut ts = String::new();
                        while !s.done() {
                            let (st, sv) = s.item()?;
                            if st == 0x40 {
                                ts = uid_string(sv);
                            }
                        }
                        pcs_ac.push(PresentationContextAc { id, result, transfer_syntax: ts });
                    }
                    0x50 => user = decode_user_info(v)?,
                    _ => {}
                }
            }
            Ok(if kind == 0x01 {
                Pdu::AssociateRq(AssociateRq { called_ae: called, calling_ae: calling, application_context: app, presentation_contexts: pcs_rq, user })
            } else {
                Pdu::AssociateAc(AssociateAc { called_ae: called, calling_ae: calling, application_context: app, presentation_contexts: pcs_ac, user })
            })
        }
        0x03 => {
            c.u8()?;
            Ok(Pdu::AssociateRj(AssociateRj { result: c.u8()?, source: c.u8()?, reason: c.u8()? }))
        }
        0x04 => {
            let mut pdvs = Vec::new();
            while !c.done() {
                let len = c.u32()? as usize;
                if len < 2 {
                    return Err(aerr("PDV item shorter than its header"));
                }
                let pc_id = c.u8()?;
                let h = c.u8()?;
                let data = c.take(len - 2)?.to_vec();
                pdvs.push(Pdv { pc_id, command: h & 1 != 0, last: h & 2 != 0, data });
            }
            Ok(Pdu::PData(pdvs))
        }
        0x05 => Ok(Pdu::ReleaseRq),
        0x06 => Ok(Pdu::ReleaseRp),
        0x07 => {
            c.take(2)?;
            Ok(Pdu::Abort { source: c.u8()?, reason: c.u8()? })
        }
        other => Err(aerr(format!("Unknown PDU type 0x{:02X}", other))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdus_round_trip() {
        let rq = Pdu::AssociateRq(AssociateRq {
            called_ae: "PACS".into(),
            calling_ae: "CAMELEER".into(),
            application_context: APPLICATION_CONTEXT.into(),
            presentation_contexts: vec![PresentationContextRq { id: 1, abstract_syntax: "1.2.840.10008.1.1".into(), transfer_syntaxes: vec!["1.2.840.10008.1.2".into(), "1.2.840.10008.1.2.1".into()] }],
            user: UserInfo {
                max_pdu_length: 16384,
                implementation_class_uid: "1.2.3".into(),
                implementation_version_name: Some("V1".into()),
                async_ops: Some((1, 1)),
                role_selections: vec![RoleSelection { sop_class: "1.2.840.10008.5.1.4.1.1.2".into(), scu: false, scp: true }],
                extended: vec![("1.2.840.10008.5.1.4.1.2.2.1".into(), vec![1])],
                user_identity: Some(UserIdentity { kind: 2, positive_response_requested: true, primary: b"user".to_vec(), secondary: b"pw".to_vec() }),
                user_identity_response: None,
            },
        });
        let ac = Pdu::AssociateAc(AssociateAc {
            called_ae: "PACS".into(),
            calling_ae: "CAMELEER".into(),
            application_context: APPLICATION_CONTEXT.into(),
            presentation_contexts: vec![PresentationContextAc { id: 1, result: 0, transfer_syntax: "1.2.840.10008.1.2".into() }],
            user: UserInfo { max_pdu_length: 0, implementation_class_uid: "1.2.4".into(), user_identity_response: Some(b"ok".to_vec()), ..Default::default() },
        });
        let pd = Pdu::PData(vec![Pdv { pc_id: 1, command: true, last: true, data: vec![1, 2, 3] }, Pdv { pc_id: 3, command: false, last: false, data: vec![] }]);
        for p in [rq, ac, pd, Pdu::AssociateRj(AssociateRj { result: 1, source: 1, reason: 7 }), Pdu::ReleaseRq, Pdu::ReleaseRp, Pdu::Abort { source: 2, reason: 6 }] {
            let b = encode(&p);
            let back = read_pdu(&mut &b[..], 0).unwrap().unwrap();
            assert_eq!(back, p);
        }
        assert!(read_pdu(&mut &[0x09u8, 0, 0, 0, 0, 0][..], 0).is_err());
        assert!(read_pdu(&mut &[0x04u8, 0, 0, 0, 0, 9, 1][..], 0).is_err());
        assert!(read_pdu(&mut &[][..], 0).unwrap().is_none());
    }
}
