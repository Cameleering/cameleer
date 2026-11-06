//! `cameleer-bench raw-http <port> threads|poll`: a minimal HTTP responder without Camel, to
//! measure what the load generator and the socket model allow on this machine (the ceiling
//! for the `http` scenario). Not part of the reported results.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\n\r\n{\"status\":\"ok\"}";

fn complete_requests(buf: &[u8]) -> usize {
    buf.windows(4).filter(|w| *w == b"\r\n\r\n").count()
}

/// Thread per connection, blocking reads (the model of cameleer-http's server).
pub fn threads(port: u16) -> std::io::Result<()> {
    let l = TcpListener::bind(("0.0.0.0", port))?;
    for s in l.incoming() {
        let mut s = s?;
        std::thread::spawn(move || {
            let _ = s.set_nodelay(true);
            let mut buf = vec![0u8; 8192];
            let mut have = 0;
            loop {
                let n = match s.read(&mut buf[have..]) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                have += n;
                let reqs = complete_requests(&buf[..have]);
                if reqs > 0 {
                    // the load generator sends one request at a time
                    have = 0;
                    for _ in 0..reqs {
                        if s.write_all(RESPONSE).is_err() {
                            return;
                        }
                    }
                }
            }
        });
    }
    Ok(())
}

#[cfg(windows)]
mod sys {
    #[repr(C)]
    pub struct PollFd {
        pub fd: usize,
        pub events: i16,
        pub revents: i16,
    }
    pub const POLLIN: i16 = 0x0100 | 0x0200; // POLLRDNORM | POLLRDBAND
    #[link(name = "ws2_32")]
    extern "system" {
        pub fn WSAPoll(fds: *mut PollFd, n: u32, timeout: i32) -> i32;
    }
    pub fn raw(s: &std::net::TcpStream) -> usize {
        use std::os::windows::io::AsRawSocket;
        s.as_raw_socket() as usize
    }
    pub unsafe fn poll(fds: *mut PollFd, n: u32, timeout: i32) -> i32 {
        WSAPoll(fds, n, timeout)
    }
}

#[cfg(unix)]
mod sys {
    #[repr(C)]
    pub struct PollFd {
        pub fd: i32,
        pub events: i16,
        pub revents: i16,
    }
    pub const POLLIN: i16 = 0x1;
    // `nfds_t` is `unsigned long` on Linux and `unsigned int` on macOS and the BSDs.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    type Nfds = std::ffi::c_ulong;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    type Nfds = std::ffi::c_uint;
    mod ffi {
        extern "C" {
            pub fn poll(fds: *mut super::PollFd, n: super::Nfds, timeout: i32) -> i32;
        }
    }
    pub fn raw(s: &std::net::TcpStream) -> i32 {
        use std::os::unix::io::AsRawFd;
        s.as_raw_fd()
    }
    pub unsafe fn poll(fds: *mut PollFd, n: u32, timeout: i32) -> i32 {
        ffi::poll(fds, n as Nfds, timeout)
    }
}

/// One thread, non-blocking sockets and `poll`: no thread wakes up per request while
/// requests keep arriving.
pub fn poll(port: u16) -> std::io::Result<()> {
    let l = TcpListener::bind(("0.0.0.0", port))?;
    l.set_nonblocking(true)?;
    let mut conns: Vec<(TcpStream, Vec<u8>)> = Vec::new();
    let mut buf = vec![0u8; 8192];
    loop {
        while let Ok((s, _)) = l.accept() {
            s.set_nonblocking(true)?;
            let _ = s.set_nodelay(true);
            conns.push((s, Vec::new()));
        }
        let mut fds: Vec<sys::PollFd> = conns.iter().map(|(s, _)| sys::PollFd { fd: sys::raw(s), events: sys::POLLIN, revents: 0 }).collect();
        if fds.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }
        // SAFETY: `fds` is a valid array of `fds.len()` entries.
        let n = unsafe { sys::poll(fds.as_mut_ptr(), fds.len() as u32, 1) };
        if n <= 0 {
            continue;
        }
        let mut closed = Vec::new();
        for (i, fd) in fds.iter().enumerate() {
            if fd.revents == 0 {
                continue;
            }
            let (s, pending) = &mut conns[i];
            loop {
                match s.read(&mut buf) {
                    Ok(0) => {
                        closed.push(i);
                        break;
                    }
                    Ok(k) => pending.extend_from_slice(&buf[..k]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        closed.push(i);
                        break;
                    }
                }
            }
            let reqs = complete_requests(pending);
            if reqs > 0 {
                pending.clear();
                for _ in 0..reqs {
                    let _ = s.write_all(RESPONSE);
                }
            }
        }
        for i in closed.into_iter().rev() {
            conns.swap_remove(i);
        }
    }
}
