//! Connection core shared by client and server: record I/O, handshake message
//! framing, alerts and post-handshake messages.

use super::keyschedule::{cipher13, next_traffic_secret};
use super::record::{RecordCipher, MAX_FRAGMENT};
use super::{alert, codec, content, hs, CipherSuite, NamedGroup, TlsError, TlsResult, TlsVersion};
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};

const MAX_HANDSHAKE_MESSAGE: usize = 1 << 18;

pub(crate) struct Conn<S> {
    pub io: S,
    rbuf: Vec<u8>,
    wbuf: Vec<u8>,
    read_cipher: Option<RecordCipher>,
    write_cipher: Option<RecordCipher>,
    /// TLS 1.2: activated when the peer's ChangeCipherSpec arrives.
    pub pending_read12: Option<RecordCipher>,
    hs_buf: Vec<u8>,
    app_buf: Vec<u8>,
    app_pos: usize,
    pub transcript: Vec<u8>,
    pub is_client: bool,
    pub version: Option<TlsVersion>,
    pub handshaking: bool,
    ccs_ignored: u32,
    pub suite: Option<CipherSuite>,
    pub group: Option<NamedGroup>,
    pub alpn: Option<Vec<u8>>,
    pub peer_certs: Vec<Vec<u8>>,
    pub sni: Option<String>,
    /// TLS 1.3 application traffic secrets (for KeyUpdate).
    pub read_secret: Vec<u8>,
    pub write_secret: Vec<u8>,
    read_closed: bool,
    write_closed: bool,
    broken: Option<String>,
    /// Set on the read half of a split connection: alerts and KeyUpdate replies go
    /// through the (shared) write half.
    peer_writer: Option<Arc<Mutex<dyn SharedWrite>>>,
}

/// Write-side operations the read half of a split connection needs.
pub(crate) trait SharedWrite: Send {
    fn shared_send_alert(&mut self, level: u8, desc: u8);
    fn shared_key_update(&mut self) -> TlsResult<()>;
    fn shared_mark_broken(&mut self, msg: &str);
}

impl<S: Read + Write + Send> SharedWrite for Conn<S> {
    fn shared_send_alert(&mut self, level: u8, desc: u8) {
        self.send_alert(level, desc);
    }
    fn shared_key_update(&mut self) -> TlsResult<()> {
        if self.write_closed || self.broken.is_some() {
            return Ok(());
        }
        self.send_key_update(false)
    }
    fn shared_mark_broken(&mut self, msg: &str) {
        if self.broken.is_none() {
            self.broken = Some(msg.to_string());
        }
    }
}

impl<S: Read + Write> Conn<S> {
    pub fn new(io: S, is_client: bool) -> Conn<S> {
        Conn {
            io,
            rbuf: Vec::new(),
            wbuf: Vec::new(),
            read_cipher: None,
            write_cipher: None,
            pending_read12: None,
            hs_buf: Vec::new(),
            app_buf: Vec::new(),
            app_pos: 0,
            transcript: Vec::new(),
            is_client,
            version: None,
            handshaking: true,
            ccs_ignored: 0,
            suite: None,
            group: None,
            alpn: None,
            peer_certs: Vec::new(),
            sni: None,
            read_secret: Vec::new(),
            write_secret: Vec::new(),
            read_closed: false,
            write_closed: false,
            broken: None,
            peer_writer: None,
        }
    }

    /// Splits an established connection into a read half (keeping `self.io`) and a
    /// write half using `write_io` (typically a clone of the same socket).
    pub fn split<W: Read + Write + Send + 'static>(mut self, write_io: W) -> TlsResult<(Conn<S>, Arc<Mutex<Conn<W>>>)> {
        if self.handshaking || self.version.is_none() {
            return Err(TlsError::Config("cannot split a connection before the handshake completed".into()));
        }
        self.flush()?;
        let mut w = Conn::new(write_io, self.is_client);
        w.write_cipher = self.write_cipher.take();
        w.write_secret = std::mem::take(&mut self.write_secret);
        w.write_closed = self.write_closed;
        w.version = self.version;
        w.handshaking = false;
        w.suite = self.suite;
        w.group = self.group;
        w.alpn = self.alpn.clone();
        w.sni = self.sni.clone();
        w.broken = self.broken.clone();
        let w = Arc::new(Mutex::new(w));
        let dynw: Arc<Mutex<dyn SharedWrite>> = w.clone();
        self.peer_writer = Some(dynw);
        self.write_closed = true;
        Ok((self, w))
    }

    // ------------------------------------------------------------ reading

    /// Ensures `n` bytes are buffered. Returns false on EOF with an empty buffer.
    fn fill(&mut self, n: usize) -> TlsResult<bool> {
        if self.rbuf.len() >= n {
            return Ok(true);
        }
        let mut tmp = [0u8; 18 * 1024];
        while self.rbuf.len() < n {
            let k = match self.io.read(&mut tmp) {
                Ok(k) => k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(TlsError::Io(e)),
            };
            if k == 0 {
                if self.rbuf.is_empty() {
                    return Ok(false);
                }
                return Err(TlsError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed in the middle of a TLS record")));
            }
            self.rbuf.extend_from_slice(&tmp[..k]);
        }
        Ok(true)
    }

    fn is_tls13(&self) -> bool {
        self.version == Some(TlsVersion::Tls13)
    }

    /// Reads and unprotects one record; returns (content type, plaintext).
    fn read_record(&mut self) -> TlsResult<(u8, Vec<u8>)> {
        if !self.fill(5)? {
            return Err(TlsError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "peer closed the connection without sending close_notify",
            )));
        }
        let mut header = [0u8; 5];
        header.copy_from_slice(&self.rbuf[..5]);
        let typ = header[0];
        if !(content::CHANGE_CIPHER_SPEC..=content::APPLICATION_DATA).contains(&typ) || header[1] != 3 {
            return Err(TlsError::proto(alert::DECODE_ERROR, "received data is not a TLS record (is the peer speaking TLS?)"));
        }
        let len = u16::from_be_bytes([header[3], header[4]]) as usize;
        if len > MAX_FRAGMENT + 2048 {
            return Err(TlsError::proto(alert::RECORD_OVERFLOW, "record too large"));
        }
        self.fill(5 + len)?;
        let body: Vec<u8> = self.rbuf[5..5 + len].to_vec();
        self.rbuf.drain(..5 + len);
        if typ == content::CHANGE_CIPHER_SPEC {
            return Ok((typ, body));
        }
        let tls13 = self.is_tls13();
        match self.read_cipher.as_mut() {
            Some(c) => {
                if tls13 && typ == content::ALERT {
                    // An unprotected alert after the key change: report it, but never treat
                    // it as close_notify (that would permit truncation attacks).
                    if body.len() == 2 && body[1] != alert::CLOSE_NOTIFY {
                        return Err(TlsError::AlertReceived(body[1]));
                    }
                    return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "unprotected alert after key change"));
                }
                c.decrypt(&header, &body)
            }
            None => {
                if len > MAX_FRAGMENT {
                    return Err(TlsError::proto(alert::RECORD_OVERFLOW, "plaintext record too large"));
                }
                if typ == content::APPLICATION_DATA {
                    return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "application data before handshake completion"));
                }
                Ok((typ, body))
            }
        }
    }

    /// Processes an alert record. Ok(true) means close_notify; Ok(false) an ignorable warning.
    fn handle_alert(&mut self, data: &[u8]) -> TlsResult<bool> {
        if data.len() != 2 {
            return Err(TlsError::proto(alert::DECODE_ERROR, "malformed alert"));
        }
        let (level, desc) = (data[0], data[1]);
        if desc == alert::CLOSE_NOTIFY {
            return Ok(true);
        }
        if desc == alert::USER_CANCELED || (level == 1 && !self.is_tls13()) {
            return Ok(false);
        }
        Err(TlsError::AlertReceived(desc))
    }

    fn take_hs_msg(&mut self) -> TlsResult<Option<Vec<u8>>> {
        if self.hs_buf.len() < 4 {
            return Ok(None);
        }
        let len = (self.hs_buf[1] as usize) << 16 | (self.hs_buf[2] as usize) << 8 | self.hs_buf[3] as usize;
        if len > MAX_HANDSHAKE_MESSAGE {
            return Err(TlsError::proto(alert::DECODE_ERROR, "handshake message too large"));
        }
        if self.hs_buf.len() < 4 + len {
            return Ok(None);
        }
        Ok(Some(self.hs_buf.drain(..4 + len).collect()))
    }

    /// Reads the next complete handshake message (header included). Not added to the transcript.
    pub fn read_hs(&mut self) -> TlsResult<Vec<u8>> {
        loop {
            if let Some(m) = self.take_hs_msg()? {
                return Ok(m);
            }
            let (typ, data) = self.read_record()?;
            match typ {
                content::HANDSHAKE => {
                    if data.is_empty() {
                        return Err(TlsError::proto(alert::DECODE_ERROR, "empty handshake record"));
                    }
                    if self.pending_read12.is_some() {
                        return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "handshake message where ChangeCipherSpec was expected"));
                    }
                    self.hs_buf.extend_from_slice(&data);
                }
                content::ALERT => {
                    if self.handle_alert(&data)? {
                        return Err(TlsError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "peer closed the connection during the handshake")));
                    }
                }
                content::CHANGE_CIPHER_SPEC => {
                    if data != [1] || !self.hs_buf.is_empty() {
                        return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "invalid ChangeCipherSpec"));
                    }
                    if self.version == Some(TlsVersion::Tls12) {
                        match self.pending_read12.take() {
                            Some(c) => self.read_cipher = Some(c),
                            None => return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "unexpected ChangeCipherSpec")),
                        }
                    } else if self.handshaking && self.ccs_ignored < 2 {
                        self.ccs_ignored += 1; // TLS 1.3 middlebox-compatibility CCS
                    } else {
                        return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "unexpected ChangeCipherSpec"));
                    }
                }
                _ => return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "application data during handshake")),
            }
        }
    }

    /// Reads a handshake message of the given type (not added to the transcript).
    pub fn expect(&mut self, typ: u8) -> TlsResult<Vec<u8>> {
        let m = self.read_hs()?;
        if m[0] != typ {
            return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, format!("expected handshake message {typ}, got {}", m[0])));
        }
        Ok(m)
    }

    /// Installs a new read key; key changes must align with record boundaries.
    pub fn set_read_cipher(&mut self, c: RecordCipher) -> TlsResult<()> {
        if !self.hs_buf.is_empty() {
            return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "handshake data not aligned with key change"));
        }
        self.read_cipher = Some(c);
        Ok(())
    }

    pub fn set_write_cipher(&mut self, c: RecordCipher) {
        self.write_cipher = Some(c);
    }

    // ------------------------------------------------------------ writing

    fn queue_record(&mut self, typ: u8, data: &[u8]) -> TlsResult<()> {
        let mut chunks: Vec<&[u8]> = data.chunks(MAX_FRAGMENT).collect();
        if chunks.is_empty() {
            chunks.push(&[]);
        }
        for chunk in chunks {
            match self.write_cipher.as_mut() {
                Some(c) => {
                    let rec = c.encrypt(typ, chunk)?;
                    self.wbuf.extend(rec);
                }
                None => {
                    self.wbuf.extend_from_slice(&[typ, 3, 3]);
                    codec::put_u16(&mut self.wbuf, chunk.len() as u16);
                    self.wbuf.extend_from_slice(chunk);
                }
            }
        }
        Ok(())
    }

    pub fn flush(&mut self) -> TlsResult<()> {
        if !self.wbuf.is_empty() {
            let buf = std::mem::take(&mut self.wbuf);
            self.io.write_all(&buf)?;
        }
        self.io.flush()?;
        Ok(())
    }

    /// Queues a handshake message and adds it to the transcript.
    pub fn send_hs(&mut self, msg: &[u8]) -> TlsResult<()> {
        self.transcript.extend_from_slice(msg);
        self.queue_record(content::HANDSHAKE, msg)
    }

    /// Queues an (always unprotected) ChangeCipherSpec record.
    pub fn send_ccs(&mut self) {
        self.wbuf.extend_from_slice(&[content::CHANGE_CIPHER_SPEC, 3, 3, 0, 1, 1]);
    }

    /// Best-effort alert transmission.
    pub fn send_alert(&mut self, level: u8, desc: u8) {
        if let Some(w) = &self.peer_writer {
            if let Ok(mut w) = w.lock() {
                w.shared_send_alert(level, desc);
            }
            return;
        }
        if self.queue_record(content::ALERT, &[level, desc]).is_ok() {
            let _ = self.flush();
        }
    }

    /// Records a fatal error: sends the matching alert (if any) and poisons the connection.
    pub fn fail(&mut self, e: TlsError) -> TlsError {
        if self.broken.is_none() {
            if let Some(a) = e.alert_to_send() {
                self.send_alert(2, a);
            }
            self.broken = Some(e.to_string());
            if let Some(w) = &self.peer_writer {
                if let Ok(mut w) = w.lock() {
                    w.shared_mark_broken(&e.to_string());
                }
            }
        }
        e
    }

    fn check_usable(&self) -> TlsResult<()> {
        match &self.broken {
            Some(m) => Err(TlsError::Io(io::Error::other(format!("TLS connection unusable after earlier error: {m}")))),
            None => Ok(()),
        }
    }

    // ------------------------------------------------------------ application data

    pub fn read_app(&mut self, buf: &mut [u8]) -> TlsResult<usize> {
        self.check_usable()?;
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.app_pos < self.app_buf.len() {
                let n = buf.len().min(self.app_buf.len() - self.app_pos);
                buf[..n].copy_from_slice(&self.app_buf[self.app_pos..self.app_pos + n]);
                self.app_pos += n;
                return Ok(n);
            }
            if self.read_closed {
                return Ok(0);
            }
            let (typ, data) = self.read_record()?;
            match typ {
                content::APPLICATION_DATA => {
                    self.app_buf = data;
                    self.app_pos = 0;
                }
                content::ALERT => {
                    if self.handle_alert(&data)? {
                        self.read_closed = true;
                        return Ok(0);
                    }
                }
                content::HANDSHAKE => {
                    if data.is_empty() {
                        return Err(TlsError::proto(alert::DECODE_ERROR, "empty handshake record"));
                    }
                    self.hs_buf.extend_from_slice(&data);
                    while let Some(m) = self.take_hs_msg()? {
                        self.post_handshake(&m)?;
                    }
                }
                _ => return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "unexpected ChangeCipherSpec")),
            }
        }
    }

    fn post_handshake(&mut self, m: &[u8]) -> TlsResult<()> {
        let body = &m[4..];
        if self.is_tls13() {
            match m[0] {
                hs::NEW_SESSION_TICKET if self.is_client => Ok(()), // resumption not supported: ignore
                hs::KEY_UPDATE => {
                    if body.len() != 1 || body[0] > 1 {
                        return Err(TlsError::proto(alert::DECODE_ERROR, "malformed KeyUpdate"));
                    }
                    if !self.hs_buf.is_empty() {
                        return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "KeyUpdate not at record boundary"));
                    }
                    let suite = self.suite.expect("suite");
                    self.read_secret = next_traffic_secret(suite.hash(), &self.read_secret);
                    self.read_cipher = Some(cipher13(suite, &self.read_secret));
                    if body[0] == 1 {
                        if let Some(w) = &self.peer_writer {
                            w.lock().map_err(|_| TlsError::Config("write half poisoned".into()))?.shared_key_update()?;
                        } else if !self.write_closed {
                            self.send_key_update(false)?;
                        }
                    }
                    Ok(())
                }
                t => Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, format!("unexpected post-handshake message {t}"))),
            }
        } else {
            match m[0] {
                hs::HELLO_REQUEST if self.is_client => {
                    self.send_alert(1, alert::NO_RENEGOTIATION);
                    Ok(())
                }
                hs::CLIENT_HELLO if !self.is_client => {
                    self.send_alert(1, alert::NO_RENEGOTIATION);
                    Ok(())
                }
                t => Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, format!("unexpected post-handshake message {t}"))),
            }
        }
    }

    /// TLS 1.3 KeyUpdate: sends the message under the current key, then rotates the write key.
    pub fn send_key_update(&mut self, request_peer: bool) -> TlsResult<()> {
        if !self.is_tls13() || self.handshaking {
            return Err(TlsError::Config("KeyUpdate requires an established TLS 1.3 connection".into()));
        }
        let msg = codec::handshake_msg(hs::KEY_UPDATE, &[request_peer as u8]);
        self.queue_record(content::HANDSHAKE, &msg)?;
        let suite = self.suite.expect("suite");
        self.write_secret = next_traffic_secret(suite.hash(), &self.write_secret);
        self.write_cipher = Some(cipher13(suite, &self.write_secret));
        self.flush()
    }

    pub fn write_app(&mut self, data: &[u8]) -> TlsResult<usize> {
        self.check_usable()?;
        if self.write_closed {
            return Err(TlsError::Closed);
        }
        // rotate keys well before AEAD usage limits (RFC 8446 section 5.5)
        if self.is_tls13() && self.write_cipher.as_ref().map(|c| c.seq).unwrap_or(0) > (1 << 24) {
            self.send_key_update(false)?;
        }
        self.queue_record(content::APPLICATION_DATA, data)?;
        self.flush()?;
        Ok(data.len())
    }

    /// Sends close_notify (idempotent).
    pub fn close(&mut self) -> TlsResult<()> {
        if self.write_closed || self.broken.is_some() {
            return Ok(());
        }
        self.write_closed = true;
        self.queue_record(content::ALERT, &[1, alert::CLOSE_NOTIFY])?;
        self.flush()
    }

    pub fn read_closed(&self) -> bool {
        self.read_closed
    }
}
