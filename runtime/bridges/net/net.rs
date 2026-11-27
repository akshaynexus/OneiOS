// SPDX-License-Identifier: MIT
//! The network bridge (C: net/net.c): the frames of the guest's en0 (virtio-net, UniXNU-droidvm's DroidVMVirtioNet.c
//! and DroidVMVirtioEthernet.cpp in the kernel) to a tap device on One UI's kernel. One UI's Wi-Fi stays associated
//! (wpa_supplicant and the vendor Wi-Fi HAL are init services and keep running in a session); the tap is routed and
//! NATed through the phone's active network by net/oneios-net.sh.
//!
//! iOS configures en0 with DHCP. No DHCP server runs in a session (Android's is in the network stack app, which is
//! gone with zygote), so the bridge answers the guest's DHCP itself: one lease, the tap's host address as router, the
//! configured DNS servers.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use xh_core::bytes::be;
use xh_core::sys::{errno, strerror};

/// The longest frame (Ethernet, no FCS).
pub const FRAME_MAX: usize = 1514;

/// The guest's en0 as the bridge sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetConfig {
    /// tap name, default "oneios0" (at most 15 bytes)
    pub ifname: String,
    /// en0's address
    pub guest_mac: [u8; 6],
    /// host byte order; defaults 192.168.77.1 / .2 / 255.255.255.0
    pub host_ip: u32,
    pub guest_ip: u32,
    pub netmask: u32,
    /// handed to iOS by the built-in DHCP answer (0: none)
    pub dns: [u32; 2],
    pub lease_s: u32,
    /// answer DHCP in the bridge (default true)
    pub dhcp: bool,
}

impl Default for NetConfig {
    fn default() -> NetConfig {
        NetConfig {
            ifname: "oneios0".to_owned(),
            guest_mac: [0; 6],
            host_ip: 0xc0a84d01,  // 192.168.77.1
            guest_ip: 0xc0a84d02, // 192.168.77.2
            netmask: 0xffffff00,
            dns: [0x08080808, 0x01010101], // public DNS: the phone's own resolver is netd's and is not read
            lease_s: 86400,
            dhcp: true,
        }
    }
}

/// Gets each frame for the guest (from the bridge's receive thread, or from send for a DHCP answer): true when the
/// guest took it, false when it was dropped (its receive ring stayed full: devices::virtio_net).
pub type RxFn = Arc<dyn Fn(&[u8]) -> bool + Send + Sync>;

/// A frame sink for the guest's en0: net's tap or fd backend, the phone's Wi-Fi (wifi::wlan), the stand-in radio.
/// Errors are positive errnos.
pub trait NetBackend: Send + Sync {
    /// A frame from the guest.
    fn send(&self, frame: &[u8]) -> Result<(), i32>;
}

/// Makes a backend with the device's receive function (`Net::open_tap` on the phone). None: no network.
pub type NetOpen = Box<dyn FnOnce(&NetConfig, RxFn) -> Option<Box<dyn NetBackend>> + Send>;

// ---- DHCP ---------------------------------------------------------------------------------------

fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = data.chunks(2).map(|w| u32::from(w[0]) << 8 | u32::from(*w.get(1).unwrap_or(&0))).sum();
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

const MAGIC: [u8; 4] = [0x63, 0x82, 0x53, 0x63];
/// The bridge's own Ethernet address in its answers.
const HOST_MAC: [u8; 6] = [0x02, 0x4f, 0x69, 0x4f, 0x53, 0x01];

/// The DHCP reply to `frame` if it is a DHCPDISCOVER/REQUEST from the guest; None when the frame is not one.
pub fn dhcp_reply(c: &NetConfig, frame: &[u8]) -> Option<Vec<u8>> {
    // Ethernet 14, IPv4 (IHL from the header), UDP 8, BOOTP 236, magic cookie 4, options.
    if frame.len() < 14 + 20 + 8 + 240 || be::u16(frame, 12) != 0x0800 {
        return None;
    }
    let ip = &frame[14..];
    let ihl = usize::from(ip[0] & 15) * 4;
    if ip[0] >> 4 != 4 || ihl < 20 || ip[9] != 17 || frame.len() < 14 + ihl + 8 + 240 {
        return None;
    }
    let udp = &ip[ihl..];
    if be::u16(udp, 0) != 68 || be::u16(udp, 2) != 67 {
        return None;
    }
    let bootp = &udp[8..];
    if bootp[0] != 1 || bootp[1] != 1 || bootp[2] != 6 || bootp[236..240] != MAGIC {
        return None;
    }
    let mut kind = 0;
    let mut i = 240;
    while i < bootp.len() {
        match bootp[i] {
            255 => break,
            0 => i += 1,
            option => {
                if i + 2 > bootp.len() || i + 2 + usize::from(bootp[i + 1]) > bootp.len() {
                    return None;
                }
                if option == 53 && bootp[i + 1] == 1 {
                    kind = bootp[i + 2];
                }
                i += 2 + usize::from(bootp[i + 1]);
            }
        }
    }
    let answer = match kind {
        1 => 2,           // DISCOVER -> OFFER
        3 => 5,           // REQUEST -> ACK
        _ => return None, // RELEASE, INFORM, DECLINE: nothing to say
    };

    let mut options = vec![53, 1, answer];
    let mut put = |code: u8, value: u32| {
        options.extend([code, 4]);
        options.extend(value.to_be_bytes());
    };
    put(54, c.host_ip);
    put(51, c.lease_s);
    put(1, c.netmask);
    put(3, c.host_ip);
    let dns: Vec<u32> = c.dns.iter().copied().filter(|&d| d != 0).collect();
    if !dns.is_empty() {
        options.extend([6, 4 * dns.len() as u8]);
        dns.iter().for_each(|d| options.extend(d.to_be_bytes()));
    }
    options.push(255);
    let bootp_out = (240 + options.len()).max(300); // BOOTP's minimum, which some clients insist on
    let mut out = vec![0u8; 14 + 20 + 8 + bootp_out];
    // The client has no address yet: answer to broadcast, as a server does when the flag asks or always may.
    out[..6].fill(0xff);
    out[6..12].copy_from_slice(&HOST_MAC);
    out[12..14].copy_from_slice(&0x0800u16.to_be_bytes());
    {
        let oip = &mut out[14..34];
        oip[0] = 0x45;
        oip[2..4].copy_from_slice(&((20 + 8 + bootp_out) as u16).to_be_bytes());
        oip[8] = 64;
        oip[9] = 17;
        oip[12..16].copy_from_slice(&c.host_ip.to_be_bytes());
        oip[16..20].copy_from_slice(&[0xff; 4]);
        let sum = checksum(oip);
        oip[10..12].copy_from_slice(&sum.to_be_bytes());
    }
    let oudp = &mut out[34..42];
    oudp[0..2].copy_from_slice(&67u16.to_be_bytes());
    oudp[2..4].copy_from_slice(&68u16.to_be_bytes());
    oudp[4..6].copy_from_slice(&((8 + bootp_out) as u16).to_be_bytes()); // checksum 0: none (IPv4)
    let ob = &mut out[42..];
    ob[..3].copy_from_slice(&[2, 1, 6]);
    ob[4..8].copy_from_slice(&bootp[4..8]); // xid
    ob[10..12].copy_from_slice(&bootp[10..12]); // flags
    ob[16..20].copy_from_slice(&c.guest_ip.to_be_bytes()); // yiaddr
    ob[20..24].copy_from_slice(&c.host_ip.to_be_bytes()); // siaddr
    ob[28..44].copy_from_slice(&bootp[28..44]); // chaddr
    ob[236..240].copy_from_slice(&MAGIC);
    ob[240..240 + options.len()].copy_from_slice(&options);
    Some(out)
}

// ---- back ends ----------------------------------------------------------------------------------
/// The tap (or a descriptor that carries one frame per read/write) and its receive thread.
pub struct Net {
    config: NetConfig,
    fd: Arc<OwnedFd>,
    rx: RxFn,
    running: Arc<AtomicBool>,
    wake: OwnedFd,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Net {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Net").field("config", &self.config).finish_non_exhaustive()
    }
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: a valid array of two descriptors for pipe2 to fill.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe2 gave two descriptors that nothing else owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn receiver(fd: Arc<OwnedFd>, wake: OwnedFd, running: Arc<AtomicBool>, rx: RxFn) {
    let mut frame = [0u8; FRAME_MAX + 64];
    let mut fds = [
        libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
    ];
    while running.load(Ordering::Acquire) {
        // SAFETY: a valid array of two pollfds.
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
            if errno() == libc::EINTR {
                continue;
            }
            break;
        }
        if fds[1].revents != 0 {
            break; // the bridge closes
        }
        if fds[0].revents & libc::POLLIN == 0 {
            if fds[0].revents != 0 {
                break;
            }
            continue;
        }
        // SAFETY: `frame` is a writable buffer of its length.
        let got = unsafe { libc::read(fd.as_raw_fd(), frame.as_mut_ptr().cast(), frame.len()) };
        if got > 0 {
            let _ = rx(&frame[..got as usize]);
        } else if got == 0 || !matches!(errno(), libc::EINTR | libc::EAGAIN) {
            break;
        }
    }
}

impl Net {
    /// A backend over a descriptor that carries one frame per read/write (tests: a socketpair).
    pub fn open_fd(c: &NetConfig, fd: OwnedFd, rx: RxFn) -> io::Result<Net> {
        let (wake_rx, wake_tx) = pipe()?;
        let fd = Arc::new(fd);
        let running = Arc::new(AtomicBool::new(true));
        let thread = {
            let (fd, running, rx) = (Arc::clone(&fd), Arc::clone(&running), Arc::clone(&rx));
            std::thread::Builder::new().name("net-rx".into()).spawn(move || receiver(fd, wake_rx, running, rx))?
        };
        Ok(Net { config: c.clone(), fd, rx, running, wake: wake_tx, thread: Some(thread) })
    }

    /// Opens the tap (does not configure addresses or routes: net/oneios-net.sh does) and starts the receive thread,
    /// which calls rx for each frame for the guest. Needs root. None: the reason is logged.
    pub fn open_tap(c: &NetConfig, rx: RxFn) -> Option<Net> {
        let open = |path: &str| std::fs::OpenOptions::new().read(true).write(true).open(path).map(OwnedFd::from);
        let fd = match open("/dev/net/tun").or_else(|_| open("/dev/tun")) {
            // Android's node is /dev/tun
            Ok(fd) => fd,
            Err(e) => {
                crate::br_error!("net: off: /dev/tun: {}", xh_core::sys::error_text(&e));
                return None;
            }
        };
        const TUNSETIFF: u32 = xh_core::ioctl::iow(b'T', 202, 4); // _IOW('T', 202, int)
        const IFF_TAP: i16 = 0x0002;
        const IFF_NO_PI: i16 = 0x1000;
        #[repr(C)]
        struct IfReq {
            name: [u8; 16],
            flags: i16,
            _pad: [u8; 22],
        }
        let mut request = IfReq { name: [0; 16], flags: IFF_TAP | IFF_NO_PI, _pad: [0; 22] };
        let name = crate::log::truncate(&c.ifname, 15).as_bytes();
        request.name[..name.len()].copy_from_slice(name);
        // SAFETY: TUNSETIFF reads and writes a struct ifreq (40 bytes), which IfReq is.
        if unsafe { xh_core::ioctl::ioctl(fd.as_raw_fd(), TUNSETIFF, &mut request) }.is_err() {
            crate::br_error!("net: off: TUNSETIFF {}: {}", c.ifname, strerror(errno()));
            return None;
        }
        let n = Net::open_fd(c, fd, rx).ok()?;
        crate::br_info!(
            "net: on: tap {} (run net/oneios-net.sh up to route it)",
            xh_core::text::nul_text(&request.name)
        );
        Some(n)
    }

    /// `open_tap` as a [`NetOpen`] for the virtio-net device.
    pub fn tap_opener() -> NetOpen {
        Box::new(|c, rx| Net::open_tap(c, rx).map(|n| Box::new(n) as Box<dyn NetBackend>))
    }
}

impl NetBackend for Net {
    /// A frame from the guest. DHCP requests are answered here when the config says dhcp.
    fn send(&self, frame: &[u8]) -> Result<(), i32> {
        if frame.len() < 14 || frame.len() > FRAME_MAX {
            return Err(libc::EINVAL);
        }
        if self.config.dhcp {
            if let Some(reply) = dhcp_reply(&self.config, frame) {
                let _ = (self.rx)(&reply);
                return Ok(());
            }
        }
        // SAFETY: `frame` is a readable buffer of its length.
        if unsafe { libc::write(self.fd.as_raw_fd(), frame.as_ptr().cast(), frame.len()) } < 0 {
            return Err(errno());
        }
        Ok(())
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        // SAFETY: one byte from a valid buffer: the thread sits in poll().
        let _ = unsafe { libc::write(self.wake.as_raw_fd(), [0u8].as_ptr().cast(), 1) };
        if let Some(t) = self.thread.take() {
            // The last owner may be a frame handed up by the receive thread itself: it ends by itself then.
            if t.thread().id() != std::thread::current().id() {
                let _ = t.join();
            }
        }
    }
}
