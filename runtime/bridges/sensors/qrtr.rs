// SPDX-License-Identifier: MIT
//! Linux's own QRTR (Qualcomm IPC Router) datagram sockets, AF_QIPCRTR (include/uapi/linux/qrtr.h): what the
//! sensors HAL on One UI uses to reach the sensor hub (SSC, the ADSP's sensors PD). No Android library: a socket,
//! sendto and recvfrom.
//!
//! The name server lives in the kernel (net/qrtr/ns.c, Linux 5.7 and later): a NEW_LOOKUP sent to the control port of
//! the local node gives one NEW_SERVER packet per matching service and then an empty one. Our ports close with the
//! socket, and the kernel then tells every remote node (DEL_CLIENT): the hub drops this client's requests. So a
//! killed xnuhost leaves no stream running on the hub.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

/// AF_QIPCRTR (include/linux/socket.h).
pub const AF_QIPCRTR: i32 = 42;
pub const PORT_CTRL: u32 = 0xffff_fffe;
pub const TYPE_NEW_SERVER: u32 = 4;
pub const TYPE_DEL_SERVER: u32 = 5;
pub const TYPE_NEW_LOOKUP: u32 = 10;
/// The largest datagram read (a jumbo report is under 64 KiB).
pub const MAX_PACKET: usize = 65536;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SockaddrQrtr {
    family: u16,
    node: u32,
    port: u32,
}

/// One server the name server knows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Server {
    pub service: u32,
    /// The instance word as the name server keeps it: version in bits 0-7, instance above.
    pub instance: u32,
    pub node: u32,
    pub port: u32,
}

impl Server {
    pub fn version(&self) -> u32 {
        self.instance & 0xff
    }
    pub fn instance_id(&self) -> u32 {
        self.instance >> 8
    }
}

/// A control packet (struct qrtr_ctrl_pkt: cmd, then service, instance, node, port), little-endian.
pub fn ctrl_packet(cmd: u32, service: u32, instance: u32) -> [u8; 20] {
    let mut b = [0u8; 20];
    b[0..4].copy_from_slice(&cmd.to_le_bytes());
    b[4..8].copy_from_slice(&service.to_le_bytes());
    b[8..12].copy_from_slice(&instance.to_le_bytes());
    b
}

/// A name server's answer: Some(Some(server)) for NEW_SERVER, Some(None) for the empty terminator, None for
/// anything else.
pub fn parse_server(d: &[u8]) -> Option<Option<Server>> {
    if d.len() < 20 {
        return None;
    }
    let w = |i: usize| u32::from_le_bytes(d[i..i + 4].try_into().unwrap());
    if w(0) != TYPE_NEW_SERVER {
        return None;
    }
    let s = Server { service: w(4), instance: w(8), node: w(12), port: w(16) };
    Some((s != Server::default()).then_some(s))
}

/// What came in.
pub struct Packet {
    pub node: u32,
    pub port: u32,
    pub len: usize,
}

/// A QRTR datagram socket with its own port.
pub struct Socket {
    fd: OwnedFd,
    /// The local node (getsockname), where the control port is.
    pub node: u32,
    pub port: u32,
}

impl Socket {
    pub fn open() -> io::Result<Socket> {
        // SAFETY: a plain socket call; the fd is owned below.
        let fd = unsafe { libc::socket(AF_QIPCRTR, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is a new descriptor of ours.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut a = SockaddrQrtr::default();
        let mut len = std::mem::size_of::<SockaddrQrtr>() as libc::socklen_t;
        // SAFETY: a is a sockaddr_qrtr of len bytes.
        if unsafe { libc::getsockname(fd.as_raw_fd(), (&mut a as *mut SockaddrQrtr).cast(), &mut len) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Socket { fd, node: a.node, port: a.port })
    }

    pub fn send(&self, node: u32, port: u32, data: &[u8]) -> io::Result<()> {
        let a = SockaddrQrtr { family: AF_QIPCRTR as u16, node, port };
        // SAFETY: data and a outlive the call.
        let n = unsafe {
            libc::sendto(
                self.fd.as_raw_fd(),
                data.as_ptr().cast(),
                data.len(),
                0,
                (&a as *const SockaddrQrtr).cast(),
                std::mem::size_of::<SockaddrQrtr>() as libc::socklen_t,
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// One datagram into `buf`, waiting at most `timeout` (None: forever). Ok(None) on the timeout.
    pub fn recv(&self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<Option<Packet>> {
        if !wait_readable(self.fd.as_raw_fd(), timeout)? {
            return Ok(None);
        }
        self.recv_now(buf).map(Some)
    }

    /// One datagram that is there (after poll said so).
    pub fn recv_now(&self, buf: &mut [u8]) -> io::Result<Packet> {
        let mut a = SockaddrQrtr::default();
        let mut len = std::mem::size_of::<SockaddrQrtr>() as libc::socklen_t;
        // SAFETY: buf and a are ours for the call.
        let n = unsafe {
            libc::recvfrom(
                self.fd.as_raw_fd(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                libc::MSG_DONTWAIT,
                (&mut a as *mut SockaddrQrtr).cast(),
                &mut len,
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Packet { node: a.node, port: a.port, len: n as usize })
    }

    /// The servers of `service` (0: every service) the name server knows now, from its NEW_SERVER packets up to the
    /// empty one. Waits at most `timeout` in all.
    pub fn lookup(&self, service: u32, timeout: Duration) -> io::Result<Vec<Server>> {
        self.send(self.node, PORT_CTRL, &ctrl_packet(TYPE_NEW_LOOKUP, service, 0))?;
        let deadline = Instant::now() + timeout;
        let mut buf = vec![0u8; 256];
        let mut out = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "the name server did not end its list"));
            }
            let Some(p) = self.recv(&mut buf, Some(left))? else { continue };
            match parse_server(&buf[..p.len]) {
                Some(Some(s)) if service == 0 || s.service == service => out.push(s),
                Some(None) => return Ok(out),
                _ => {}
            }
        }
    }
}

impl AsRawFd for Socket {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

/// poll() for input on one descriptor: true when readable (or in error: the read says which).
pub fn wait_readable(fd: RawFd, timeout: Option<Duration>) -> io::Result<bool> {
    let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    let ms = timeout.map_or(-1, |t| t.as_millis().min(i32::MAX as u128) as i32);
    loop {
        // SAFETY: one pollfd of ours.
        let n = unsafe { libc::poll(&mut p, 1, ms) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        return Ok(n > 0);
    }
}

/// Every server, for `sensorctl qrtr` (one socket, closed after).
pub fn services(timeout: Duration) -> io::Result<(u32, Vec<Server>)> {
    let s = Socket::open()?;
    let list = s.lookup(0, timeout)?;
    Ok((s.node, list))
}

/// A well-known QMI service's name (Linux's qrtr-lookup table, the ones a Snapdragon phone has).
pub fn service_name(service: u32) -> &'static str {
    match service {
        1 => "wds",
        2 => "dms",
        3 => "nas",
        15 => "loc",
        16 => "pds",
        24 => "pdc",
        26 => "rfrpe",
        33 => "ims settings",
        34 => "audio stream",
        41 => "rfsa",
        42 => "csd",
        43 => "slimbus",
        48 => "audio core",
        49 => "ssctl",
        50 => "thermal",
        51 => "sar",
        52 => "uim remote",
        64 => "servreg locator",
        65 => "servreg notifier",
        66 => "tmd",
        69 => "wlfw",
        71 => "pdmapper",
        74 => "rmtfs",
        400 => "SNS client (sensors: SEE)",
        _ => "",
    }
}
