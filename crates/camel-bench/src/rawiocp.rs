//! `camel-bench raw-http <port> iocp`: a do-nothing HTTP responder on Windows I/O
//! completion ports, to measure the ceiling of an event-driven server on this machine.

#[cfg(windows)]
pub fn iocp(port: u16, threads: usize) -> std::io::Result<()> {
    use std::ffi::c_void;
    use std::io::Write;
    use std::net::TcpListener;
    use std::os::windows::io::AsRawSocket;

    type Handle = *mut c_void;
    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u64,
        event: Handle,
    }
    #[repr(C)]
    struct Entry {
        key: usize,
        overlapped: *mut Overlapped,
        internal: usize,
        bytes: u32,
    }
    #[repr(C)]
    struct WsaBuf {
        len: u32,
        buf: *mut u8,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateIoCompletionPort(file: Handle, port: Handle, key: usize, threads: u32) -> Handle;
        fn GetQueuedCompletionStatusEx(port: Handle, entries: *mut Entry, count: u32, removed: *mut u32, ms: u32, alertable: i32) -> i32;
    }
    #[link(name = "ws2_32")]
    extern "system" {
        fn WSARecv(s: usize, bufs: *mut WsaBuf, n: u32, got: *mut u32, flags: *mut u32, ov: *mut Overlapped, cr: *mut c_void) -> i32;
        fn WSAGetLastError() -> i32;
    }
    #[repr(C)]
    struct Conn {
        ov: Overlapped,
        stream: std::net::TcpStream,
        buf: [u8; 8192],
        have: usize,
    }
    unsafe fn post(c: *mut Conn) -> bool {
        let conn = &mut *c;
        let mut wb = WsaBuf { len: (8192 - conn.have) as u32, buf: conn.buf.as_mut_ptr().add(conn.have) };
        let (mut got, mut flags) = (0u32, 0u32);
        conn.ov = Overlapped { internal: 0, internal_high: 0, offset: 0, event: std::ptr::null_mut() };
        let r = WSARecv(conn.stream.as_raw_socket() as usize, &mut wb, 1, &mut got, &mut flags, &mut conn.ov, std::ptr::null_mut());
        r == 0 || WSAGetLastError() == 997
    }
    const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\n\r\n{\"status\":\"ok\"}";

    struct Port(Handle);
    unsafe impl Send for Port {}
    unsafe impl Sync for Port {}
    // SAFETY: plain completion-port calls; connections are boxed and freed when closed.
    let port_h = unsafe { CreateIoCompletionPort(-1isize as Handle, std::ptr::null_mut(), 0, threads as u32) };
    let port_h = std::sync::Arc::new(Port(port_h));
    for _ in 0..threads {
        let p = port_h.clone();
        std::thread::spawn(move || {
            let mut entries: Vec<Entry> = (0..64).map(|_| Entry { key: 0, overlapped: std::ptr::null_mut(), internal: 0, bytes: 0 }).collect();
            loop {
                let mut n = 0u32;
                // SAFETY: entries has 64 slots.
                if unsafe { GetQueuedCompletionStatusEx(p.0, entries.as_mut_ptr(), 64, &mut n, u32::MAX, 0) } == 0 {
                    continue;
                }
                for e in &entries[..n as usize] {
                    let c = e.key as *mut Conn;
                    // SAFETY: the key is a live boxed connection with one receive posted.
                    unsafe {
                        if e.bytes == 0 {
                            drop(Box::from_raw(c));
                            continue;
                        }
                        let conn = &mut *c;
                        conn.have += e.bytes as usize;
                        let reqs = conn.buf[..conn.have].windows(4).filter(|w| *w == b"\r\n\r\n").count();
                        if reqs > 0 {
                            conn.have = 0;
                            for _ in 0..reqs {
                                let _ = (&conn.stream).write_all(RESPONSE);
                            }
                        }
                        if !post(c) {
                            drop(Box::from_raw(c));
                        }
                    }
                }
            }
        });
    }
    let l = TcpListener::bind(("127.0.0.1", port))?;
    for s in l.incoming() {
        let s = s?;
        let _ = s.set_nodelay(true);
        let c = Box::into_raw(Box::new(Conn { ov: Overlapped { internal: 0, internal_high: 0, offset: 0, event: std::ptr::null_mut() }, stream: s, buf: [0; 8192], have: 0 }));
        // SAFETY: associates the socket with the port, keyed by the connection.
        unsafe {
            CreateIoCompletionPort((&*c).stream.as_raw_socket() as Handle, port_h.0, c as usize, 0);
            if !post(c) {
                drop(Box::from_raw(c));
            }
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn iocp(_port: u16, _threads: usize) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "IOCP is Windows-only"))
}
