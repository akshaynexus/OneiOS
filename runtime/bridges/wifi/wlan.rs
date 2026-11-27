// SPDX-License-Identifier: MIT
//! The Wi-Fi bridge's back end on the phone (docs/hardware.md section 3): the guest's en0 on wlan0.
//!   frames   AF_PACKET on wlan0 (all types, our own sends not read back, coalesced frames cut by frames::segment;
//!            a frame of the guest's that finds the send buffer full waits in uplink for POLLOUT, 20 ms at most)
//!   scans    nl80211 (generic netlink) on wlan0, as wificond does: TRIGGER_SCAN, then GET_SCAN on NEW_SCAN_RESULTS;
//!            while joined, within 120 s of the last scan, the station answers from its networks (JOINED_SCAN_REUSE_S)
//!   link     nl80211 GET_STATION and GET_INTERFACE while joined: signal, rate, channel
//!   joins    the vendor's wpa_supplicant through its AIDL service (supplicant)
//!   host     wlan0's state as One UI left it, kept in a file and given back ([`restore`]): GRO, IPv6 off, and its
//!            IPv4 addresses removed for the session (one IP stack on the link, iOS's)
//! Threads: frames (wlan0 to the guest), events (nl80211's scan group), control (the guest's station messages, the
//! scans, the supplicant: everything that can wait). The guest's vCPU only sends a frame or queues a message.
//! And every radio and Wonder's session for `wifictl p2p` and `wifictl wonder` ([`radios`], [`PhoneWonder`]).
//! C: wifi/wlan.c.

use super::frames::{self, DhcpWho};
use super::nl;
use super::p2p::{Radios, OUI_GOOGLE};
use super::proto::*;
use super::station::{Bss, Link, Radio, Station};
use super::supplicant::{Event, Supplicant};
use super::traffic::{self, Counts, Traffic};
use super::uplink::{self, Sent, Uplink};
use super::wonder::WonderOps;
use super::{CONTROL_TYPE, FRAME};
use crate::hal::binder::{last_errno, strerror};
use crate::net::{NetBackend, NetConfig, NetOpen, RxFn};
use crate::switches::JoinedScan;
use crate::{br_error, br_info, br_log};
use std::collections::VecDeque;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use xh_core::bytes::ne;
use xh_core::thread::lock;

// ---- netlink and nl80211's numbers (linux/netlink.h, genetlink.h, nl80211.h; a test checks them) ----
const NETLINK_GENERIC: i32 = 16;
const NLM_F_REQUEST: u16 = 1;
const NLM_F_MULTI: u16 = 2;
const NLM_F_ACK: u16 = 4;
const NLM_F_DUMP: u16 = 0x300;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLMSG_HDRLEN: usize = 16;
const GENL_HDRLEN: usize = 4;
const GENL_ID_CTRL: u16 = 0x10;
const CTRL_CMD_GETFAMILY: u8 = 3;
const CTRL_ATTR_FAMILY_ID: u16 = 1;
const CTRL_ATTR_FAMILY_NAME: u16 = 2;
const CTRL_ATTR_MCAST_GROUPS: u16 = 7;
const SOL_NETLINK: i32 = 270;
const NETLINK_ADD_MEMBERSHIP: i32 = 1;

const CMD_GET_WIPHY: u8 = 1;
const CMD_GET_INTERFACE: u8 = 5;
const CMD_NEW_INTERFACE: u8 = 7;
const CMD_DEL_INTERFACE: u8 = 8;
const CMD_GET_STATION: u8 = 17;
const CMD_GET_REG: u8 = 31;
const CMD_GET_SCAN: u8 = 32;
const CMD_TRIGGER_SCAN: u8 = 33;
const CMD_NEW_SCAN_RESULTS: u8 = 34;
const CMD_SCAN_ABORTED: u8 = 35;
const CMD_VENDOR: u8 = 103;
const ATTR_WIPHY: u16 = 1;
const ATTR_IFINDEX: u16 = 3;
const ATTR_IFNAME: u16 = 4;
const ATTR_IFTYPE: u16 = 5;
const ATTR_MAC: u16 = 6;
const ATTR_STA_INFO: u16 = 21;
const ATTR_WIPHY_BANDS: u16 = 22;
const ATTR_REG_ALPHA2: u16 = 33;
const ATTR_WIPHY_FREQ: u16 = 38;
const ATTR_SCAN_SSIDS: u16 = 45;
const ATTR_SCAN_FLAGS: u16 = 158;
/// NL80211_SCAN_FLAG_LOW_PRIORITY: the driver may cut the scan short or pause it for traffic; LOW_SPAN: fewer or
/// shorter dwells off the home channel (the wiphy says which it takes; cfg80211 refuses the others: EOPNOTSUPP)
const SCAN_FLAG_LOW_PRIORITY: u32 = 1 << 0;
const SCAN_FLAG_LOW_SPAN: u32 = 1 << 8;
const ATTR_BSS: u16 = 47;
const ATTR_SPLIT_WIPHY_DUMP: u16 = 174;
const ATTR_VENDOR_ID: u16 = 195;
const ATTR_VENDOR_SUBCMD: u16 = 196;
const ATTR_VENDOR_DATA: u16 = 197;
const BSS_BSSID: u16 = 1;
const BSS_FREQUENCY: u16 = 2;
const BSS_BEACON_INTERVAL: u16 = 4;
const BSS_CAPABILITY: u16 = 5;
const BSS_INFORMATION_ELEMENTS: u16 = 6;
const BSS_SIGNAL_MBM: u16 = 7;
const BSS_SEEN_MS_AGO: u16 = 10;
const STA_INFO_SIGNAL: u16 = 7;
const STA_INFO_TX_BITRATE: u16 = 8;
const RATE_INFO_BITRATE: u16 = 1;
const RATE_INFO_BITRATE32: u16 = 5;
const IFTYPE_MONITOR: u32 = 6;

// AF_PACKET (linux/if_packet.h), the virtio-net header (linux/virtio_net.h), ethtool (linux/ethtool.h, sockios.h)
const ETH_P_ALL: u16 = 3;
const SOL_PACKET: i32 = 263;
const PACKET_ADD_MEMBERSHIP: i32 = 1;
const PACKET_VNET_HDR: i32 = 15;
const PACKET_IGNORE_OUTGOING: i32 = 23;
const PACKET_MR_ALLMULTI: u16 = 2;
const VNET_HDR: usize = 10; // struct virtio_net_hdr
const VIRTIO_NET_HDR_F_NEEDS_CSUM: u8 = 1;
const VIRTIO_NET_HDR_GSO_NONE: u8 = 0;
const SIOCETHTOOL: u32 = 0x8946;
const SIOCGIFFLAGS: u32 = 0x8913;
const SIOCSIFFLAGS: u32 = 0x8914;
const ETHTOOL_GGRO: u32 = 0x2b;
const ETHTOOL_SGRO: u32 = 0x2c;

const CT_QUEUE: usize = 32;
const JOIN_TIMEOUT_MS: u64 = 25000; // the guest's driver gives up at 30 s
const SCAN_TIMEOUT_MS: u64 = 8000; // the guest's at 10 s
const LINK_POLL_MS: u64 = 2000;
const TRAFFIC_REPORT_S: u64 = 60; // the `wifi: 60 s:` line
/// The AF_PACKET socket's receive buffer: what comes from wlan0 while the guest's ring is full waits here
/// (devices::virtio_net waits for the guest's buffers). The default (rmem_default, about 200 KiB) holds about 80
/// coalesced-and-cut frames; 4 MiB holds a TCP window of a fast link.
const PACKET_RCVBUF: i32 = 4 << 20;
/// Its send buffer: the guest's frames that wlan0's driver has not sent yet count against it (each about 1-2 KiB
/// of the kernel's memory, a TCP ACK too: an estimate), and a send past it is EAGAIN. The default (wmem_default,
/// about 200 KiB) held about 100 of them: the phone's session of 2026-11-23 19:40 had 1624 such sends a minute while
/// a download ran. 1 MiB asked (the kernel doubles it) holds about 1000; a frame that still finds it full waits
/// (wifi::uplink).
const PACKET_SNDBUF: i32 = 1 << 20;
/// Scans iOS asks while joined are answered from the last scan's networks for this long after it (station.rs,
/// [`Radio::joined_scan_reuse`]): wifid asks one about every 35 s while joined, and each takes the radio off the
/// network's channel for about 5 s on this phone, whose driver takes neither low-priority flag (2026-11-23 19:40:
/// `scans while joined: flags 0x0`). 120 s: one scan of every channel in about 140 s instead of one in 35.
const JOINED_SCAN_REUSE_S: u64 = 120;

fn socket(domain: i32, kind: i32, protocol: i32) -> Result<OwnedFd, i32> {
    // SAFETY: a new socket, owned below.
    let fd = unsafe { libc::socket(domain, kind | libc::SOCK_CLOEXEC, protocol) };
    if fd < 0 {
        return Err(last_errno());
    }
    // SAFETY: our new fd.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Waits up to `ms` (-1: for ever) for any of `fds` to be readable: their revents, None on an error but EINTR (then
/// all zero).
fn poll(fds: &[RawFd], ms: i32) -> Result<Vec<i16>, i32> {
    let mut p: Vec<libc::pollfd> =
        fds.iter().map(|&fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
    // SAFETY: our pollfds.
    let n = unsafe { libc::poll(p.as_mut_ptr(), p.len() as libc::nfds_t, ms) };
    if n < 0 {
        let rc = last_errno();
        return if rc == -libc::EINTR { Ok(vec![0; fds.len()]) } else { Err(rc) };
    }
    Ok(p.iter().map(|p| p.revents).collect())
}

fn recv(fd: RawFd, buf: &mut [u8], flags: i32) -> Result<usize, i32> {
    // SAFETY: a buffer of ours of its length.
    let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), flags) };
    if n < 0 {
        Err(last_errno())
    } else {
        Ok(n as usize)
    }
}

/// Each netlink message of a datagram: (type, flags, seq, the message).
fn messages(buf: &[u8]) -> impl Iterator<Item = (u16, u16, u32, &[u8])> {
    let mut rest = buf;
    std::iter::from_fn(move || {
        if rest.len() < NLMSG_HDRLEN {
            return None;
        }
        let len = u32::from_ne_bytes(rest[..4].try_into().expect("4 bytes")) as usize;
        if len < NLMSG_HDRLEN || len > rest.len() {
            return None;
        }
        let m = &rest[..len];
        let kind = u16::from_ne_bytes([m[4], m[5]]);
        let flags = u16::from_ne_bytes([m[6], m[7]]);
        let seq = ne::u32(m, 8);
        let step = (len + 3) & !3;
        rest = if step >= rest.len() { &[] } else { &rest[step..] };
        Some((kind, flags, seq, m))
    })
}

/// A generic netlink socket for nl80211's commands.
struct Nl80211 {
    genl: OwnedFd,
    seq: u32,
    family: u16,
    buffer: Vec<u8>,
}

impl Nl80211 {
    /// The command socket and the socket of nl80211's "scan" multicast group.
    fn open() -> Result<(Nl80211, OwnedFd), i32> {
        let genl = socket(libc::AF_NETLINK, libc::SOCK_RAW, NETLINK_GENERIC)?;
        let events = socket(libc::AF_NETLINK, libc::SOCK_RAW, NETLINK_GENERIC)?;
        for fd in [&genl, &events] {
            // SAFETY: a zeroed sockaddr_nl with its family: the kernel picks our port.
            let mut local: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
            local.nl_family = libc::AF_NETLINK as libc::sa_family_t;
            // SAFETY: our socket and a sockaddr_nl of its size.
            let rc = unsafe {
                libc::bind(
                    fd.as_raw_fd(),
                    (&local as *const libc::sockaddr_nl).cast(),
                    std::mem::size_of::<libc::sockaddr_nl>() as u32,
                )
            };
            if rc != 0 {
                return Err(last_errno());
            }
        }
        let mut nl = Nl80211 { genl, seq: 0, family: GENL_ID_CTRL, buffer: vec![0; 65536] };
        let mut w = nl::Writer::new(64);
        w.attr(CTRL_ATTR_FAMILY_NAME, b"nl80211\0");
        let (mut id, mut group) = (0u16, 0u32);
        nl.call_to(GENL_ID_CTRL, CTRL_CMD_GETFAMILY, 0, w.finish().unwrap_or_default(), |attrs| {
            for (kind, data) in nl::attrs(attrs) {
                match kind {
                    CTRL_ATTR_FAMILY_ID => id = nl::u16_of(data).unwrap_or(id),
                    // One multicast group of the family: its id and its name, in the order the kernel wrote them.
                    CTRL_ATTR_MCAST_GROUPS => {
                        if let Some(g) = nl::attrs(data).find_map(|(_, entry)| frames::nl_group(entry, "scan")) {
                            group = g;
                        }
                    }
                    _ => {}
                }
            }
        })?;
        if id == 0 || group == 0 {
            return Err(-libc::ENOENT);
        }
        nl.family = id;
        // SAFETY: NETLINK_ADD_MEMBERSHIP reads one u32 group id.
        let rc = unsafe {
            libc::setsockopt(events.as_raw_fd(), SOL_NETLINK, NETLINK_ADD_MEMBERSHIP, (&group as *const u32).cast(), 4)
        };
        if rc != 0 {
            return Err(last_errno());
        }
        Ok((nl, events))
    }

    fn request(&mut self, family: u16, cmd: u8, flags: u16, attrs: &[u8]) -> Result<(), i32> {
        let len = NLMSG_HDRLEN + GENL_HDRLEN + attrs.len();
        if len > 512 {
            return Err(-libc::E2BIG);
        }
        self.seq = self.seq.wrapping_add(1);
        let mut msg = Vec::with_capacity(len);
        msg.extend_from_slice(&(len as u32).to_ne_bytes());
        msg.extend_from_slice(&family.to_ne_bytes());
        msg.extend_from_slice(&(NLM_F_REQUEST | flags).to_ne_bytes());
        msg.extend_from_slice(&self.seq.to_ne_bytes());
        msg.extend_from_slice(&0u32.to_ne_bytes());
        msg.extend_from_slice(&[cmd, 1, 0, 0]); // genlmsghdr: cmd, version 1
        msg.extend_from_slice(attrs);
        // SAFETY: our socket and message.
        if unsafe { libc::send(self.genl.as_raw_fd(), msg.as_ptr().cast(), msg.len(), 0) } < 0 {
            return Err(last_errno());
        }
        Ok(())
    }

    /// Reads replies to the last request until its end (DONE or an ACK/error); `each` gets each message's
    /// attributes.
    fn replies(&mut self, mut each: impl FnMut(&[u8])) -> Result<(), i32> {
        loop {
            if poll(&[self.genl.as_raw_fd()], 3000)?[0] == 0 {
                return Err(-libc::ETIMEDOUT);
            }
            let got = match recv(self.genl.as_raw_fd(), &mut self.buffer, 0) {
                Err(rc) if rc == -libc::EINTR => continue,
                r => r?,
            };
            for (kind, flags, seq, m) in messages(&self.buffer[..got]) {
                if seq != self.seq {
                    continue;
                }
                match kind {
                    NLMSG_DONE => return Ok(()),
                    NLMSG_ERROR => {
                        let e =
                            m.get(16..20).map_or(-libc::EPROTO, |b| i32::from_ne_bytes(b.try_into().expect("4 bytes")));
                        return if e == 0 { Ok(()) } else { Err(e) };
                    }
                    _ => {}
                }
                if m.len() >= NLMSG_HDRLEN + GENL_HDRLEN {
                    each(&m[NLMSG_HDRLEN + GENL_HDRLEN..]);
                }
                if flags & NLM_F_MULTI == 0 {
                    return Ok(());
                }
            }
        }
    }

    fn call_to(&mut self, family: u16, cmd: u8, flags: u16, attrs: &[u8], each: impl FnMut(&[u8])) -> Result<(), i32> {
        self.request(family, cmd, flags, attrs)?;
        self.replies(each)
    }

    /// An nl80211 command and its replies.
    fn call(&mut self, cmd: u8, flags: u16, attrs: &[u8], each: impl FnMut(&[u8])) -> Result<(), i32> {
        self.call_to(self.family, cmd, flags, attrs, each)
    }
}

fn ifindex_attrs(ifindex: u32) -> Vec<u8> {
    let mut w = nl::Writer::new(16);
    w.u32(ATTR_IFINDEX, ifindex);
    w.finish().unwrap_or_default().to_vec()
}

/// An NL80211_ATTR_BSS: the record and its elements; None without a BSSID.
fn bss_of(data: &[u8]) -> Option<Bss> {
    let mut b = OwBss::default();
    let mut ies: &[u8] = &[];
    let mut have = false;
    for (kind, v) in nl::attrs(data) {
        match kind {
            BSS_BSSID if v.len() == 6 => {
                b.bssid.copy_from_slice(v);
                have = true;
            }
            BSS_FREQUENCY => b.freq_mhz = nl::u32_of(v).unwrap_or(b.freq_mhz),
            BSS_BEACON_INTERVAL => b.beacon_interval = nl::u16_of(v).unwrap_or(b.beacon_interval),
            BSS_CAPABILITY => b.capability = nl::u16_of(v).unwrap_or(b.capability),
            BSS_SIGNAL_MBM => {
                if let Some(mbm) = nl::u32_of(v) {
                    b.signal_dbm = (mbm as i32 / 100) as i16;
                }
            }
            BSS_SEEN_MS_AGO => b.age_ms = nl::u32_of(v).unwrap_or(b.age_ms),
            BSS_INFORMATION_ELEMENTS => ies = v,
            _ => {}
        }
    }
    if !have {
        return None;
    }
    // The SSID element (id 0) gives the name.
    let mut i = 0;
    while i + 2 <= ies.len() {
        let (id, len) = (ies[i], ies[i + 1] as usize);
        if i + 2 + len > ies.len() {
            break;
        }
        if id == 0 && len <= 32 {
            b.ssid_length = len as u8;
            b.ssid[..len].copy_from_slice(&ies[i + 2..i + 2 + len]);
            break;
        }
        i += 2 + len;
    }
    Some(Bss { bss: b, ies: ies.to_vec() })
}

/// The networks the driver has for the interface: each to `each`; their number.
fn dump_scan(nl: &mut Nl80211, ifindex: u32, mut each: impl FnMut(&OwBss, &[u8])) -> Result<usize, i32> {
    let mut count = 0;
    nl.call(CMD_GET_SCAN, NLM_F_DUMP, &ifindex_attrs(ifindex), |attrs| {
        for (_, data) in nl::attrs(attrs).filter(|(t, _)| *t == ATTR_BSS) {
            if let Some(b) = bss_of(data) {
                count += 1;
                each(&b.bss, &b.ies);
            }
        }
    })?;
    Ok(count)
}

/// An active scan of every channel the driver scans by default: one wildcard SSID, as wificond asks. `flags`: the
/// scan's NL80211_ATTR_SCAN_FLAGS (0: none).
fn trigger_scan(nl: &mut Nl80211, ifindex: u32, flags: u32) -> Result<(), i32> {
    let mut wildcard = nl::Writer::new(8);
    wildcard.flag(1);
    let mut w = nl::Writer::new(40);
    w.u32(ATTR_IFINDEX, ifindex).attr(ATTR_SCAN_SSIDS, wildcard.finish().unwrap_or_default());
    if flags != 0 {
        w.u32(ATTR_SCAN_FLAGS, flags);
    }
    nl.call(CMD_TRIGGER_SCAN, NLM_F_ACK, w.finish().unwrap_or_default(), |_| {})
}

/// The flags of a scan while joined, best first: low priority and low span, low priority, none. The radio of the
/// session's network goes off its channel for each channel scanned; iOS's wifid asks a full scan about every 35 s
/// while joined (the phone, 2026-11-23: `sets 10`, then `event 10` 5 s later with 29 networks, each time), and a
/// low-priority scan is one the driver may pause or cut short for the link's traffic.
pub(crate) const JOINED_SCAN_FLAGS: [u32; 3] = [SCAN_FLAG_LOW_PRIORITY | SCAN_FLAG_LOW_SPAN, SCAN_FLAG_LOW_PRIORITY, 0];

/// The flags to try from `start` on (an index of [`JOINED_SCAN_FLAGS`]) after the driver answered `rc`: the next
/// index when it refused the flags (cfg80211 answers a flag the wiphy does not offer with EOPNOTSUPP, an older
/// kernel an unknown attribute with EINVAL), None when that answer is final.
pub(crate) fn joined_scan_next(at: usize, rc: Result<(), i32>) -> Option<usize> {
    match rc {
        Err(e) if (e == -libc::EOPNOTSUPP || e == -libc::EINVAL) && at + 1 < JOINED_SCAN_FLAGS.len() => Some(at + 1),
        _ => None,
    }
}

/// A station entry's fields (a station's only peer is its access point): its address, signal and rate.
fn station_attrs(attrs: &[u8], l: &mut Link) {
    for (kind, data) in nl::attrs(attrs) {
        match kind {
            ATTR_MAC if data.len() == 6 => l.bssid.copy_from_slice(data),
            ATTR_STA_INFO => {
                for (t, v) in nl::attrs(data) {
                    match t {
                        STA_INFO_SIGNAL if !v.is_empty() => l.rssi_dbm = i32::from(v[0] as i8),
                        STA_INFO_TX_BITRATE => {
                            for (r, rv) in nl::attrs(v) {
                                match r {
                                    RATE_INFO_BITRATE32 if rv.len() == 4 => {
                                        l.tx_rate_mbps = nl::u32_of(rv).unwrap_or(0) / 10
                                    }
                                    RATE_INFO_BITRATE if rv.len() == 2 && l.tx_rate_mbps == 0 => {
                                        l.tx_rate_mbps = u32::from(nl::u16_of(rv).unwrap_or(0)) / 10
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

fn interface_freq(attrs: &[u8], l: &mut Link) {
    if let Some(f) =
        nl::attrs(attrs).find(|(t, d)| *t == ATTR_WIPHY_FREQ && d.len() == 4).and_then(|(_, d)| nl::u32_of(d))
    {
        l.freq_mhz = f;
    }
}

/// The interface's index and address (/sys/class/net/IFACE/address); None when it is not there.
fn interface(ifname: &str) -> Option<(u32, [u8; 6])> {
    let c = CString::new(ifname).ok()?;
    // SAFETY: a NUL-terminated name.
    let index = unsafe { libc::if_nametoindex(c.as_ptr()) };
    let text = std::fs::read_to_string(format!("/sys/class/net/{ifname}/address")).ok()?;
    let parts: Vec<u8> = text.trim().split(':').filter_map(|p| u8::from_str_radix(p, 16).ok()).collect();
    (index != 0 && parts.len() == 6).then(|| (index, parts.try_into().expect("6 bytes")))
}

/// The interface exists (its index and address readable): the chip's HAL starts the one that does not.
pub fn present(ifname: &str) -> bool {
    interface(ifname).is_some()
}

/// The interface and nl80211, with their failure logged (C: wlan_new).
fn open_interface(ifname: &str) -> Option<(String, u32, [u8; 6], Nl80211, OwnedFd)> {
    let name: String = if ifname.is_empty() { "wlan0".into() } else { ifname.chars().take(15).collect() };
    let Some((ifindex, mac)) = interface(&name) else {
        br_error!("wifi: off: no {name} (One UI's Wi-Fi was off at the switch?): iOS gets no Wi-Fi");
        return None;
    };
    match Nl80211::open() {
        Ok((nl, events)) => Some((name, ifindex, mac, nl, events)),
        Err(rc) => {
            br_error!("wifi: off: nl80211: {}", strerror(rc));
            None
        }
    }
}

// ---- the control thread's messages ----
enum Ct {
    Guest(Vec<u8>),
    ScanReady,
    ScanAborted,
    Supplicant(Event),
    Quit,
}

/// The control thread's own state.
#[derive(Default)]
struct Control {
    scanning: bool,
    joining: bool,
    joined: bool,
    reason: u32, // why the link went down
    scan_deadline: Option<Instant>,
    join_deadline: Option<Instant>,
    joined_rx: u64, // the counters at the join, for the report 10 s after it
    joined_tx: u64,
    report_at: Option<Instant>,
    traffic_at: Option<Instant>, // the next `wifi: 60 s:` line
    traffic_last: Counts,        // the counts at the last one
}

/// The session's Wi-Fi: everything the three threads and the guest's sends share.
struct Wlan {
    ifname: String,
    ifindex: u32,
    mac: [u8; 6],
    nl: Mutex<Nl80211>, // the control thread's (and prepare's before it starts)
    events: OwnedFd,    // nl80211 scan events: the events thread's
    packet: OwnedFd,    // AF_PACKET
    wake: (OwnedFd, OwnedFd),
    kick: OwnedFd, // an eventfd: a frame of the guest's waits for the send buffer (the frames thread watches POLLOUT)
    uplink: Uplink, // the guest's frames that wait for the send buffer
    running: AtomicBool,
    state_path: Option<String>,
    station: Station,
    rx: OnceLock<RxFn>,
    ctl: Mutex<Control>,
    supplicant: Mutex<Option<Supplicant>>,
    // The control thread: everything that is not a frame. The guest's vCPU, the nl80211 events and the supplicant's
    // callbacks only put a message here, so none of them waits on netlink, on binder or on the guest's queues.
    queue: Mutex<VecDeque<Ct>>,
    more: Condvar,
    traffic: Traffic,
    control_dropped: AtomicU64,
    joined_scan_flags: AtomicUsize, // the index in JOINED_SCAN_FLAGS of the flags the driver took last
    dhcp_said: AtomicU32,           // iOS's DHCP messages logged since the last join (the first of them)
    dhcp_others: AtomicU32,         // other stations' DHCP messages seen since the last join (the first is logged)
}

impl Wlan {
    fn post(&self, m: Ct) {
        let mut q = lock(&self.queue);
        if q.len() == CT_QUEUE {
            self.control_dropped.fetch_add(1, Ordering::Relaxed);
        } else {
            q.push_back(m);
            self.more.notify_one();
        }
    }

    /// A frame for the guest: true when it took it (false: its receive ring stayed full, or no device yet).
    fn deliver(&self, frame: &[u8]) -> bool {
        self.rx.get().is_some_and(|rx| rx(frame))
    }

    fn finish_scan(&self, error: i32) {
        if !std::mem::replace(&mut lock(&self.ctl).scanning, false) {
            return;
        }
        let rc = if error != 0 {
            Err(error)
        } else {
            dump_scan(&mut lock(&self.nl), self.ifindex, |b, ies| self.station.scan_add(b, ies))
        };
        self.station.scan_done(rc.err().unwrap_or(0));
    }

    /// The supplicant's verdicts (on its looper, posted to the control thread).
    fn control_supplicant(&self, e: Event) {
        let leave = |w: &Wlan| {
            if let Some(s) = lock(&w.supplicant).as_mut() {
                let _ = s.leave();
            }
        };
        match e {
            Event::Joined => {
                {
                    let mut c = lock(&self.ctl);
                    if !c.joining {
                        return;
                    }
                    c.joining = false;
                    c.joined = true;
                    c.reason = 0;
                    c.joined_rx = self.traffic.rx_frames.load(Ordering::Relaxed);
                    c.joined_tx = self.traffic.tx_frames.load(Ordering::Relaxed);
                    c.report_at = Some(Instant::now() + Duration::from_secs(10));
                }
                self.dhcp_said.store(0, Ordering::Relaxed);
                self.dhcp_others.store(0, Ordering::Relaxed);
                self.station.join_done(OW_JOIN_OK, 0);
            }
            Event::JoinFailed { status, code } => {
                if !std::mem::replace(&mut lock(&self.ctl).joining, false) {
                    return;
                }
                leave(self); // or it tries the network again by itself
                self.station.join_done(status, code);
            }
            Event::Disconnected { .. } | Event::Died => {
                // iOS decides whether to join again: the network goes out of the supplicant, which would come back
                // on its own.
                let code = if let Event::Disconnected { code } = e { code } else { 0 };
                if matches!(e, Event::Disconnected { .. }) {
                    leave(self);
                }
                if std::mem::replace(&mut lock(&self.ctl).joining, false) {
                    self.station.join_done(OW_JOIN_FAILED, code);
                }
                let down = {
                    let mut c = lock(&self.ctl);
                    let was = std::mem::replace(&mut c.joined, false);
                    if was {
                        c.reason = if code != 0 { code } else { 1 };
                    }
                    was.then_some(c.reason)
                };
                if let Some(reason) = down {
                    br_info!("wifi: link down (reason {reason})");
                    self.station.link_changed();
                }
            }
        }
    }

    fn control_loop(self: &Arc<Self>) {
        // The supplicant first: a lazy service, started by this lookup when One UI's framework took it down with itself.
        let me = Arc::downgrade(self);
        let s = Supplicant::open(
            &self.ifname,
            8000,
            Some(Box::new(move |e| {
                if let Some(w) = me.upgrade() {
                    w.post(Ct::Supplicant(e))
                }
            })),
        );
        br_log!(
            if s.is_some() { crate::log::Level::Info } else { crate::log::Level::Error },
            "wifi: {}",
            if s.is_some() {
                "the vendor's supplicant is the bridge's: joins by its AIDL service"
            } else {
                "no supplicant: iOS can scan but not join"
            }
        );
        *lock(&self.supplicant) = s;
        let mut poll_at = Instant::now() + Duration::from_millis(LINK_POLL_MS);
        loop {
            let m = {
                let q = lock(&self.queue);
                let busy = {
                    let c = lock(&self.ctl);
                    c.joining || c.scanning
                };
                let wait = Duration::from_millis(if busy { 250 } else { LINK_POLL_MS });
                let (mut q, _) =
                    self.more.wait_timeout_while(q, wait, |q| q.is_empty()).unwrap_or_else(PoisonError::into_inner);
                q.pop_front()
            };
            match m {
                Some(Ct::Quit) => break,
                Some(Ct::Guest(mut frame)) => {
                    self.station.message(&frame);
                    frame.fill(0); // a join's message carried the key
                }
                Some(Ct::ScanReady) => self.finish_scan(0),
                Some(Ct::ScanAborted) => self.finish_scan(-libc::ECANCELED),
                Some(Ct::Supplicant(e)) => self.control_supplicant(e),
                None => {}
            }
            let now = Instant::now();
            let (scan_late, join_late) = {
                let c = lock(&self.ctl);
                (
                    c.scanning && c.scan_deadline.is_some_and(|d| now >= d),
                    c.joining && c.join_deadline.is_some_and(|d| now >= d),
                )
            };
            if scan_late {
                self.finish_scan(0); // no event: what the driver has
            }
            if join_late {
                lock(&self.ctl).joining = false;
                br_error!("wifi: join: not joined in {} s: given up", JOIN_TIMEOUT_MS / 1000);
                if let Some(s) = lock(&self.supplicant).as_mut() {
                    let _ = s.leave();
                }
                self.station.join_done(OW_JOIN_TIMEOUT, 0);
            }
            if now >= poll_at {
                poll_at = now + Duration::from_millis(LINK_POLL_MS);
                if lock(&self.ctl).joined {
                    self.station.link_changed(); // the signal
                }
            }
            let report = {
                let mut c = lock(&self.ctl);
                match c.report_at {
                    Some(at) if now >= at => {
                        c.report_at = None; // did iOS's traffic start: its DHCP lines are above this one
                        c.joined.then_some((c.joined_tx, c.joined_rx))
                    }
                    _ => None,
                }
            };
            if let Some((tx, rx)) = report {
                let t = self.traffic.read();
                br_info!(
                    "wifi: 10 s after the join: {} frames from iOS, {} to it; {} DHCP messages of other stations",
                    t.tx_frames - tx,
                    t.rx_frames - rx,
                    self.dhcp_others.load(Ordering::Relaxed)
                );
            }
            self.traffic_report(now);
        }
        lock(&self.supplicant).take(); // leaves the network, drops what it added
    }

    /// The `wifi: 60 s:` line (traffic::line), on the control thread's periodic wake: none while nothing moved.
    fn traffic_report(&self, now: Instant) {
        let mut c = lock(&self.ctl);
        let at = *c.traffic_at.get_or_insert(now + Duration::from_secs(TRAFFIC_REPORT_S));
        if now < at {
            return;
        }
        c.traffic_at = Some(now + Duration::from_secs(TRAFFIC_REPORT_S));
        self.traffic.joined_cached.store(self.station.cached_scans(), Ordering::Relaxed);
        let counts = self.traffic.read();
        let ring = crate::devices::virtio_net::take_ring_waits();
        let sent_longest = self.traffic.tx_wait_longest_us.swap(0, Ordering::Relaxed);
        if let Some(line) = traffic::line(TRAFFIC_REPORT_S, &c.traffic_last, &counts, ring, sent_longest) {
            br_info!("{line}");
        }
        c.traffic_last = counts;
    }

    /// The scan event group: NEW_SCAN_RESULTS (ours or anyone's) and SCAN_ABORTED, for our interface.
    fn events_loop(&self) {
        let mut buffer = vec![0u8; 8192];
        while self.running.load(Ordering::Acquire) {
            let Ok(r) = poll(&[self.events.as_raw_fd(), self.wake.0.as_raw_fd()], -1) else { break };
            if r[1] != 0 {
                break;
            }
            let Ok(got) = recv(self.events.as_raw_fd(), &mut buffer, libc::MSG_DONTWAIT) else { continue };
            let family = lock(&self.nl).family;
            for (kind, _, _, m) in messages(&buffer[..got]) {
                if m.len() < NLMSG_HDRLEN + GENL_HDRLEN || kind != family {
                    continue;
                }
                match m[NLMSG_HDRLEN] {
                    CMD_NEW_SCAN_RESULTS => self.post(Ct::ScanReady),
                    CMD_SCAN_ABORTED => self.post(Ct::ScanAborted),
                    _ => {}
                }
            }
        }
    }

    /// iOS's address configuration as it crosses the bridge: each DHCP message of a join's first ones, one line. Only
    /// iOS's own exchange is logged and counted against that limit: a message whose client (chaddr) is another
    /// station's (its broadcast DISCOVER, INIT-REBOOT or REBINDING REQUEST reaches every station, iOS too) is
    /// counted, the first after a join named once. Before 2026-11-13 every one of them was logged as a server's
    /// answer with the yiaddr a REQUEST never has ("DHCP: REQUEST of 0.0.0.0 from 0.0.0.0", repeated on a busy hotel
    /// network).
    fn say_dhcp(&self, frame: &[u8], from_guest: bool) {
        let Some(m) = frames::dhcp(frame) else { return };
        let (who, line) = m.describe(from_guest, &self.mac);
        if who == DhcpWho::Other {
            if self.dhcp_others.fetch_add(1, Ordering::Relaxed) == 0 {
                br_info!("wifi: DHCP: {line} (passed to iOS as any broadcast; such messages are counted, not logged)");
            }
            return;
        }
        if self.dhcp_said.load(Ordering::Relaxed) >= 16 {
            return;
        }
        self.dhcp_said.fetch_add(1, Ordering::Relaxed);
        br_info!("wifi: DHCP: {line}");
    }

    /// wlan0's frames to the guest, and the guest's frames that wait for the socket's send buffer (wifi::uplink):
    /// while any waits, the poll also watches POLLOUT and ends at the oldest one's bound.
    fn frames_loop(&self) {
        let mut buffer = vec![0u8; VNET_HDR + 65536];
        while self.running.load(Ordering::Acquire) {
            let deadline = self.uplink.deadline();
            let out = if deadline.is_some() { libc::POLLOUT } else { 0 };
            let mut fds = [
                libc::pollfd { fd: self.packet.as_raw_fd(), events: libc::POLLIN | out, revents: 0 },
                libc::pollfd { fd: self.wake.0.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: self.kick.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            ];
            // SAFETY: our pollfds.
            let n = unsafe { libc::poll(fds.as_mut_ptr(), 3, uplink::poll_ms(Instant::now(), deadline)) };
            if n < 0 {
                if last_errno() == -libc::EINTR {
                    continue;
                }
                break;
            }
            if fds[1].revents != 0 {
                break;
            }
            if fds[2].revents & libc::POLLIN != 0 {
                let mut v = [0u8; 8];
                // SAFETY: our eventfd and an 8-byte buffer (its count, reset by the read).
                let _ = unsafe { libc::read(self.kick.as_raw_fd(), v.as_mut_ptr().cast(), 8) };
            }
            if let Some(d) = deadline {
                let now = Instant::now();
                if fds[0].revents & libc::POLLOUT != 0 || now >= d {
                    self.uplink.flush(now, VNET_HDR, &self.traffic, |p| self.packet_send(p));
                }
            }
            // POLLERR too: a recv takes the socket's error (ENETDOWN when wlan0 went down), or poll would answer at
            // once again and again
            if fds[0].revents & (libc::POLLIN | libc::POLLERR) != 0 {
                self.receive(&mut buffer);
            }
        }
    }

    /// One frame of wlan0's for the guest (cut when the kernel coalesced it).
    fn receive(&self, buffer: &mut [u8]) {
        let got = match recv(self.packet.as_raw_fd(), buffer, libc::MSG_DONTWAIT) {
            Ok(n) if n >= VNET_HDR + 14 => n,
            Ok(_) => return,
            Err(rc) if rc == -libc::EAGAIN || rc == -libc::EINTR => return,
            Err(rc) => {
                // EINVAL: the kernel could not describe a coalesced frame in a virtio_net_hdr and the frame is lost:
                // One UI's 6.1 kernel writes one only for TCP (virtio_net_hdr_from_skb; UDP_L4 came in 6.2), and
                // GRO is off on wlan0 (no TCP frame came coalesced in the session of 2026-11-23 19:40: `0 cut`), so
                // these are UDP frames the driver itself joined [I: Qualcomm's FISA, the driver's own UDP
                // aggregation, which ethtool's GRO switch does not turn off; QUIC]. Counted for the 60 s line.
                Traffic::add(&self.traffic.rx_errors, 1);
                self.traffic.rx_last_errno.store(-rc, Ordering::Relaxed);
                return;
            }
        };
        let (flags, gso_type, gso_size) = (buffer[0], buffer[1], u16::from_ne_bytes([buffer[4], buffer[5]]));
        let frame = &buffer[VNET_HDR..got];
        if !frames::to_guest(frame, &self.mac) {
            return;
        }
        self.say_dhcp(frame, false);
        // A frame the receive path coalesced (GRO was on, or came back on) has no checksum of its own any more.
        let coalesced = gso_type != VIRTIO_NET_HDR_GSO_NONE || flags & VIRTIO_NET_HDR_F_NEEDS_CSUM != 0;
        let mss = if gso_type != VIRTIO_NET_HDR_GSO_NONE { gso_size } else { 1448 };
        let (mut taken, mut bytes, mut full) = (0u64, 0u64, 0u64);
        let n = frames::segment(frame, mss, coalesced, |f| {
            if self.deliver(f) {
                taken += 1;
                bytes += f.len() as u64;
            } else {
                full += 1;
            }
        });
        let t = &self.traffic;
        if n == 0 {
            Traffic::add(&t.rx_too_long, 1);
        } else if n > 1 {
            Traffic::add(&t.rx_cut, 1);
        }
        Traffic::add(&t.rx_frames, taken);
        Traffic::add(&t.rx_bytes, bytes);
        Traffic::add(&t.rx_ring_full, full);
    }

    /// One packet (the virtio_net_hdr and the frame) on the AF_PACKET socket, never waiting. Errors are positive
    /// errnos (EAGAIN: the send buffer is full).
    fn packet_send(&self, packet: &[u8]) -> Result<(), i32> {
        // SAFETY: our socket and buffer.
        if unsafe { libc::send(self.packet.as_raw_fd(), packet.as_ptr().cast(), packet.len(), libc::MSG_DONTWAIT) } < 0
        {
            return Err(-last_errno());
        }
        Ok(())
    }

    /// A frame of the guest's (the virtio-net device's send, on a vCPU). Errors are positive errnos (NetBackend's).
    /// A frame that finds the send buffer full waits in [`Wlan::uplink`] (Ok): the vCPU does not wait.
    fn send(&self, frame: &[u8]) -> Result<(), i32> {
        if frames::ethertype(frame) == CONTROL_TYPE {
            if frame.len() > 256 {
                return Err(libc::EMSGSIZE);
            }
            self.post(Ct::Guest(frame.to_vec()));
            return Ok(());
        }
        if !frames::to_radio(frame, &self.mac) {
            Traffic::add(&self.traffic.tx_refused, 1);
            return Err(libc::EINVAL);
        }
        self.say_dhcp(frame, true);
        let mut packet = [0u8; VNET_HDR + FRAME];
        packet[VNET_HDR..VNET_HDR + frame.len()].copy_from_slice(frame);
        let packet = &packet[..VNET_HDR + frame.len()];
        match self.uplink.send(packet, VNET_HDR, Instant::now(), &self.traffic, |p| self.packet_send(p))? {
            Sent::Now | Sent::Held { first: false } => {}
            Sent::Held { first: true } => {
                // the frames thread starts to watch POLLOUT
                let one = 1u64.to_ne_bytes();
                // SAFETY: our eventfd and its 8-byte count.
                let _ = unsafe { libc::write(self.kick.as_raw_fd(), one.as_ptr().cast(), 8) };
            }
        }
        Ok(())
    }
}

/// The station's radio: the session's Wi-Fi (scans by nl80211, joins by the supplicant).
struct WlanRadio(Weak<Wlan>);

impl Radio for WlanRadio {
    fn scan(&self, _st: &Station) -> Result<(), i32> {
        let Some(w) = self.0.upgrade() else { return Err(-libc::ENODEV) };
        // While joined: a low-priority scan, with the flags the driver took the last time (JOINED_SCAN_FLAGS).
        let joined = lock(&w.ctl).joined;
        // wifi-joined-scan=plain (docs/switches.md): no flags while joined either, as before cbf97ea.
        let plain = crate::switches::wifi().joined_scan == JoinedScan::Plain;
        let rc = if joined && plain {
            Traffic::add(&w.traffic.joined_scans, 1);
            w.traffic.joined_scan_flags.store(0, Ordering::Relaxed);
            trigger_scan(&mut lock(&w.nl), w.ifindex, 0)
        } else if joined {
            let mut at = w.joined_scan_flags.load(Ordering::Relaxed);
            let mut nl = lock(&w.nl);
            let rc = loop {
                let rc = trigger_scan(&mut nl, w.ifindex, JOINED_SCAN_FLAGS[at]);
                match joined_scan_next(at, rc) {
                    Some(next) => at = next,
                    None => break rc,
                }
            };
            drop(nl);
            if w.joined_scan_flags.swap(at, Ordering::Relaxed) != at
                || w.traffic.joined_scans.load(Ordering::Relaxed) == 0
            {
                br_info!("wifi: scans while joined: flags 0x{:x} (low priority 0x1, low span 0x100; the ones the driver takes)", JOINED_SCAN_FLAGS[at]);
            }
            Traffic::add(&w.traffic.joined_scans, 1);
            w.traffic.joined_scan_flags.store(u64::from(JOINED_SCAN_FLAGS[at]), Ordering::Relaxed);
            rc
        } else {
            trigger_scan(&mut lock(&w.nl), w.ifindex, 0)
        };
        // Busy: a scan of the supplicant's (or the driver's) runs; its results come with NEW_SCAN_RESULTS all the same.
        match rc {
            Ok(()) => {}
            Err(rc) if rc == -libc::EBUSY => {}
            Err(rc) => {
                br_error!("wifi: nl80211 TRIGGER_SCAN on {}: {}", w.ifname, strerror(rc));
                return Err(rc);
            }
        }
        let mut c = lock(&w.ctl);
        c.scanning = true;
        c.scan_deadline = Some(Instant::now() + Duration::from_millis(SCAN_TIMEOUT_MS));
        Ok(())
    }

    fn join(&self, _st: &Station, join: &OwJoin, bss: Option<&Bss>) -> Result<(), i32> {
        let Some(w) = self.0.upgrade() else { return Err(-libc::ENODEV) };
        lock(&w.ctl).joined = false;
        let rc = match lock(&w.supplicant).as_mut() {
            None => return Err(-libc::ENODEV),
            Some(s) => s.join(join, bss.map(|b| &b.ies[..(b.bss.ie_length as usize).min(b.ies.len())])),
        };
        let mut c = lock(&w.ctl);
        c.joining = rc.is_ok();
        if rc.is_ok() {
            c.join_deadline = Some(Instant::now() + Duration::from_millis(JOIN_TIMEOUT_MS));
        }
        rc
    }

    fn leave(&self, _st: &Station) -> Result<(), i32> {
        let Some(w) = self.0.upgrade() else { return Ok(()) };
        {
            let mut c = lock(&w.ctl);
            c.joining = false;
            c.joined = false;
            c.reason = 3; // the station left
        }
        let rc = lock(&w.supplicant).as_mut().map_or(Ok(()), |s| s.leave());
        rc
    }

    /// Up while the supplicant says the network is joined; the signal, the rate and the access point from nl80211's
    /// station entry, the channel from the interface.
    fn link(&self) -> Result<Link, i32> {
        let Some(w) = self.0.upgrade() else { return Err(-libc::ENODEV) };
        let (joined, reason) = {
            let c = lock(&w.ctl);
            (c.joined, c.reason)
        };
        let mut l = Link { noise_dbm: -95, up: joined, reason, ..Default::default() };
        if !joined {
            return Ok(l);
        }
        let attrs = ifindex_attrs(w.ifindex);
        {
            let mut nl = lock(&w.nl);
            let _ = nl.call(CMD_GET_STATION, NLM_F_DUMP, &attrs, |a| station_attrs(a, &mut l));
            let _ = nl.call(CMD_GET_INTERFACE, 0, &attrs, |a| interface_freq(a, &mut l));
        }
        if l.freq_mhz == 0 {
            if let Some(s) = lock(&w.supplicant).as_ref() {
                l.freq_mhz = s.frequency();
            }
        }
        if l.rssi_dbm == 0 {
            l.rssi_dbm = -60; // no station entry yet: a middling signal, not "none"
        }
        Ok(l)
    }

    /// While joined, a scan within JOINED_SCAN_REUSE_S of the last one is answered from its networks.
    /// wifi-joined-scan=low or plain (docs/switches.md): never; every scan is the radio's, as before b704922.
    fn joined_scan_reuse(&self) -> Option<Duration> {
        (crate::switches::wifi().joined_scan == JoinedScan::Cache).then(|| Duration::from_secs(JOINED_SCAN_REUSE_S))
    }

    /// The channels wlan0's radio may use: a split wiphy dump for wlan0 (NL80211_ATTR_SPLIT_WIPHY_DUMP, which newer
    /// kernels need for the bands; they come over several messages), each message's bands read by nl_bands.
    fn channels(&self, max: usize) -> Option<Result<Vec<OwChannel>, i32>> {
        let w = self.0.upgrade()?;
        let mut a = nl::Writer::new(32);
        a.u32(ATTR_IFINDEX, w.ifindex).flag(ATTR_SPLIT_WIPHY_DUMP);
        let mut out = Vec::new();
        let rc = lock(&w.nl).call(CMD_GET_WIPHY, NLM_F_DUMP, a.finish().unwrap_or_default(), |attrs| {
            for (_, bands) in nl::attrs(attrs).filter(|(t, _)| *t == ATTR_WIPHY_BANDS) {
                frames::nl_bands(bands, &mut out, max);
            }
        });
        Some(rc.map(|()| out))
    }
}

// ---- wlan0's host state -------------------------------------------------------------------------
/// A /proc/sys integer (one number and a line break), `fallback` when it cannot be read.
fn read_int(path: &str, fallback: i32) -> i32 {
    xh_core::fs::read_i64(path, 10).map_or(fallback, |v| v as i32)
}
fn write_int(path: &str, value: i32) {
    let _ = xh_core::fs::write_text(path, &format!("{value}\n"));
}

/// An ifreq: the name, then `data` at the union (a pointer or the flags).
fn ifreq(ifname: &str) -> [u8; 40] {
    let mut r = [0u8; 40];
    let n = ifname.len().min(15);
    r[..n].copy_from_slice(&ifname.as_bytes()[..n]);
    r
}

fn ioctl_ifreq(request: u32, r: &mut [u8; 40]) -> Result<(), i32> {
    let fd = socket(libc::AF_INET, libc::SOCK_DGRAM, 0)?;
    // SAFETY: an ifreq of its full size (40 bytes on LP64) for an interface request.
    unsafe { xh_core::ioctl::ioctl(fd.as_raw_fd(), request, r.as_mut_ptr()) }.map(drop).map_err(|_| last_errno())
}

/// GRO of the interface: read (None) or set; its value.
fn gro(ifname: &str, set: Option<bool>) -> Result<bool, i32> {
    // struct ethtool_value { u32 cmd; u32 data; }
    let mut v = [0u32; 2];
    v[0] = if set.is_some() { ETHTOOL_SGRO } else { ETHTOOL_GGRO };
    v[1] = u32::from(set == Some(true));
    let mut r = ifreq(ifname);
    r[16..24].copy_from_slice(&(v.as_mut_ptr() as u64).to_ne_bytes());
    ioctl_ifreq(SIOCETHTOOL, &mut r)?;
    Ok(v[1] != 0)
}

/// The state file: (ifname, disable_ipv6, gro); None when it is missing (Err(true)) or not one (Err(false)).
fn read_state(path: &str, mut v6: i32, mut g: i32) -> Result<(String, i32, i32), bool> {
    let text = std::fs::read_to_string(path).map_err(|e| e.kind() == std::io::ErrorKind::NotFound)?;
    let mut ifname = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("ifname=") {
            ifname = v.split_whitespace().next().unwrap_or("").chars().take(15).collect();
        } else if let Some(v) = line.strip_prefix("disable_ipv6=") {
            v6 = v.trim().parse().unwrap_or(v6);
        } else if let Some(v) = line.strip_prefix("gro=") {
            g = v.trim().parse().unwrap_or(g);
        }
    }
    if ifname.is_empty() || ifname.contains('/') {
        return Err(false);
    }
    Ok((ifname, v6, g))
}

/// What One UI left, written before anything changes; [`restore`] (the stop, and `wifictl restore` after a killed
/// xnuhost) reads it. Two settings only, and neither breaks One UI's Wi-Fi when it is never put back: IPv6 off is
/// how the framework itself leaves wlan0 between networks (it turns it on when it joins), GRO off only costs
/// receive speed.
fn host_take(ifname: &str, state_path: Option<&str>) {
    let p = format!("/proc/sys/net/ipv6/conf/{ifname}/disable_ipv6");
    let mut v6 = read_int(&p, 0);
    let mut g = gro(ifname, None).map_or(1, i32::from);
    // A file that is there already is a start after a restart in place: its values are One UI's, the current ones ours.
    let kept = state_path.and_then(|s| read_state(s, v6, g).ok()).filter(|(old, _, _)| old == ifname);
    match (&kept, state_path) {
        (Some((_, ov6, og)), _) => {
            v6 = *ov6;
            g = *og;
        }
        (None, Some(path)) => {
            let _ = std::fs::write(path, format!("ifname={ifname}\ndisable_ipv6={v6}\ngro={g}\n"));
        }
        (None, None) => {}
    }
    write_int(&p, 1);
    let _ = gro(ifname, Some(false));
    // One IP stack on the link: One UI's IPv4 addresses on wlan0 go (its framework adds them again after the return).
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("PATH=/system/bin:$PATH ip -4 addr flush dev {ifname} >/dev/null 2>&1"))
        .status();
    br_info!(
        "wifi: {ifname} is iOS's for the session: IPv4 addresses removed, IPv6 off (was {}), GRO off (was {}); the old values are in {}",
        if v6 != 0 { "off" } else { "on" },
        if g != 0 { "on" } else { "off" },
        state_path.unwrap_or("(nowhere: no session folder)")
    );
}

/// wlan0's settings back from a state file (the stop; `wifictl restore` after a killed xnuhost). -ENOENT: nothing to
/// give back; -EINVAL: not a state file.
pub fn restore(state_path: &str) -> Result<(), i32> {
    let (ifname, v6, g) =
        read_state(state_path, 0, 1).map_err(|missing| if missing { -libc::ENOENT } else { -libc::EINVAL })?;
    write_int(&format!("/proc/sys/net/ipv6/conf/{ifname}/disable_ipv6"), v6);
    let _ = gro(&ifname, Some(g != 0));
    let _ = std::fs::remove_file(state_path);
    br_info!(
        "wifi: {ifname} given back: IPv6 {}, GRO {}",
        if v6 != 0 { "off" } else { "on" },
        if g != 0 { "on" } else { "off" }
    );
    Ok(())
}

// ---- the back end -------------------------------------------------------------------------------
fn packet_open(ifname: &str, ifindex: u32) -> Result<OwnedFd, i32> {
    let fd = socket(libc::AF_PACKET, libc::SOCK_RAW, i32::from(ETH_P_ALL.to_be()))?;
    let on: i32 = 1;
    for opt in [PACKET_VNET_HDR, PACKET_IGNORE_OUTGOING] {
        // SAFETY: an int option of our socket.
        if unsafe { libc::setsockopt(fd.as_raw_fd(), SOL_PACKET, opt, (&on as *const i32).cast(), 4) } != 0 {
            return Err(last_errno());
        }
    }
    bind_packet(&fd, ifindex)?;
    // Deep buffers (PACKET_RCVBUF, PACKET_SNDBUF): SO_RCVBUFFORCE and SO_SNDBUFFORCE as root, past rmem_max and
    // wmem_max; SO_RCVBUF and SO_SNDBUF (capped) otherwise. What the kernel gave is logged.
    // wifi-buffers=0 (docs/switches.md): the kernel's defaults, as before cbf97ea.
    let deep = crate::switches::wifi().buffers;
    let asked = [
        ([libc::SO_RCVBUFFORCE, libc::SO_RCVBUF], PACKET_RCVBUF),
        ([libc::SO_SNDBUFFORCE, libc::SO_SNDBUF], PACKET_SNDBUF),
    ];
    for (opts, size) in if deep { &asked[..] } else { &[] } {
        let (opts, size) = (*opts, *size);
        for opt in opts {
            // SAFETY: an int option of our socket.
            if unsafe { libc::setsockopt(fd.as_raw_fd(), libc::SOL_SOCKET, opt, (&size as *const i32).cast(), 4) } == 0
            {
                break;
            }
        }
    }
    let kib = |opt| {
        let (mut v, mut n) = (0i32, 4u32);
        // SAFETY: an int option of our socket and its length.
        let rc =
            unsafe { libc::getsockopt(fd.as_raw_fd(), libc::SOL_SOCKET, opt, (&mut v as *mut i32).cast(), &mut n) };
        if rc == 0 {
            v / 1024
        } else {
            0
        }
    };
    if deep {
        br_info!(
            "wifi: {ifname}'s AF_PACKET socket: receive buffer {} KiB, send buffer {} KiB ({} KiB and {} KiB asked; the kernel doubles them)",
            kib(libc::SO_RCVBUF),
            kib(libc::SO_SNDBUF),
            PACKET_RCVBUF / 1024,
            PACKET_SNDBUF / 1024
        );
    } else {
        br_info!(
            "wifi: {ifname}'s AF_PACKET socket: receive buffer {} KiB, send buffer {} KiB (wifi-buffers=0: the kernel's defaults)",
            kib(libc::SO_RCVBUF),
            kib(libc::SO_SNDBUF)
        );
    }
    // Every multicast group: iOS's IPv6 neighbour discovery (its solicited-node group) and mDNS are groups Linux has
    // not joined on wlan0, and the driver passes up only the groups of the interface's list. The membership is the
    // socket's: it ends when the socket closes, also after a kill.
    // struct packet_mreq { int mr_ifindex; unsigned short mr_type, mr_alen; unsigned char mr_address[8]; }
    let mut mreq = [0u8; 16];
    mreq[..4].copy_from_slice(&(ifindex as i32).to_ne_bytes());
    mreq[4..6].copy_from_slice(&PACKET_MR_ALLMULTI.to_ne_bytes());
    // SAFETY: a packet_mreq of its size.
    if unsafe { libc::setsockopt(fd.as_raw_fd(), SOL_PACKET, PACKET_ADD_MEMBERSHIP, mreq.as_ptr().cast(), 16) } != 0 {
        br_error!(
            "wifi: {ifname}: all-multicast not set ({}): IPv6 neighbour discovery and mDNS may not reach iOS",
            strerror(last_errno())
        );
    }
    Ok(fd)
}

fn bind_packet(fd: &OwnedFd, ifindex: u32) -> Result<(), i32> {
    // SAFETY: a zeroed sockaddr_ll, filled below.
    let mut ll: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
    ll.sll_family = libc::AF_PACKET as u16;
    ll.sll_protocol = ETH_P_ALL.to_be();
    ll.sll_ifindex = ifindex as i32;
    // SAFETY: our socket and a sockaddr_ll of its size.
    let rc = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&ll as *const libc::sockaddr_ll).cast(),
            std::mem::size_of::<libc::sockaddr_ll>() as u32,
        )
    };
    if rc != 0 {
        return Err(last_errno());
    }
    Ok(())
}

struct Session {
    wlan: Arc<Wlan>,
    threads: Vec<JoinHandle<()>>,
}

static THE_WLAN: Mutex<Option<Session>> = Mutex::new(None);

/// The guest's sends on the session's Wi-Fi.
struct WlanNet(Arc<Wlan>);

impl NetBackend for WlanNet {
    fn send(&self, frame: &[u8]) -> Result<(), i32> {
        self.0.send(frame)
    }
}

fn pipe() -> Result<(OwnedFd, OwnedFd), i32> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: pipe2 writes two fds, owned below.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(last_errno());
    }
    // SAFETY: two new fds of ours.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// The phone's Wi-Fi as the virtio-net device's back end: wlan0 is iOS's for the session. Takes the interface
/// (`session_dir`/wifi.state keeps the two settings it changes: IPv6 and GRO) and gives the open and its address,
/// which the guest's en0 must use. None: no such interface or no access: iOS gets no Wi-Fi (logged).
pub fn prepare(ifname: &str, session_dir: Option<&str>) -> Option<(NetOpen, [u8; 6])> {
    if lock(&THE_WLAN).is_some() {
        return None; // one session has it: its chip, started or adopted, is that session's
    }
    let prepared = prepare_wlan(ifname, session_dir);
    if prepared.is_none() {
        // A chip this attempt started (and no session uses) goes down again: no half-taken radio.
        super::chip::give_back();
    }
    prepared
}

fn prepare_wlan(ifname: &str, session_dir: Option<&str>) -> Option<(NetOpen, [u8; 6])> {
    // One UI's Wi-Fi off at the switch: its chip is stopped and init never started the Wi-Fi stack, so wlan0 is not
    // there. The HAL can start it, as One UI's framework would (docs/hardware.md section 3, "Wi-Fi off in One UI").
    let name: String = if ifname.is_empty() { "wlan0".into() } else { ifname.chars().take(15).collect() };
    if interface(&name).is_none() && !super::chip::ensure(&name, super::chip::START_MS, || interface(&name).is_some()) {
        br_error!(
            "wifi: off: no {name} (One UI's Wi-Fi was off at the switch, and the chip did not start): iOS gets no Wi-Fi"
        );
        return None;
    }
    let (ifname, ifindex, mac, nl, events) = open_interface(ifname)?;
    let packet = packet_open(&ifname, ifindex)
        .inspect_err(|rc| br_error!("wifi: off: AF_PACKET on {ifname}: {}", strerror(*rc)))
        .ok()?;
    let wake = pipe().ok()?;
    // SAFETY: a new eventfd, owned below.
    let kick = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    if kick < 0 {
        return None;
    }
    // SAFETY: our new fd.
    let kick = unsafe { OwnedFd::from_raw_fd(kick) };
    let state_path = session_dir.filter(|d| !d.is_empty()).map(|d| format!("{d}/wifi.state"));
    host_take(&ifname, state_path.as_deref());
    let wlan = Arc::new_cyclic(|me: &Weak<Wlan>| {
        let deliver = me.clone();
        Wlan {
            station: Station::new(
                Box::new(WlanRadio(me.clone())),
                mac,
                None,
                Box::new(move |f: &[u8]| {
                    if let Some(w) = deliver.upgrade() {
                        let _ = w.deliver(f);
                    }
                }),
            ),
            ifname: ifname.clone(),
            ifindex,
            mac,
            nl: Mutex::new(nl),
            events,
            packet,
            wake,
            kick,
            uplink: Uplink::new(crate::switches::wifi().send_wait),
            running: AtomicBool::new(false),
            state_path,
            rx: OnceLock::new(),
            ctl: Mutex::new(Control::default()),
            supplicant: Mutex::new(None),
            queue: Mutex::new(VecDeque::new()),
            more: Condvar::new(),
            traffic: Traffic::default(),
            control_dropped: AtomicU64::new(0),
            joined_scan_flags: AtomicUsize::new(0),
            dhcp_said: AtomicU32::new(0),
            dhcp_others: AtomicU32::new(0),
        }
    });
    *lock(&THE_WLAN) = Some(Session { wlan: wlan.clone(), threads: Vec::new() });
    br_info!(
        "wifi: on: {ifname} ({:02x}:{:02x}:{:02x}:..) is the guest's en0: scans by nl80211, joins by the vendor's supplicant, frames by AF_PACKET",
        mac[0],
        mac[1],
        mac[2]
    );
    let open: NetOpen = Box::new(move |_c: &NetConfig, rx: RxFn| {
        let _ = wlan.rx.set(rx);
        wlan.running.store(true, Ordering::Release);
        let spawn = |f: fn(&Arc<Wlan>)| {
            let w = wlan.clone();
            std::thread::Builder::new().spawn(move || f(&w)).ok()
        };
        let threads: Vec<Option<JoinHandle<()>>> =
            vec![spawn(|w| w.frames_loop()), spawn(|w| w.events_loop()), spawn(|w| w.control_loop())];
        let failed = threads.iter().any(Option::is_none);
        if let Some(s) = lock(&THE_WLAN).as_mut() {
            s.threads = threads.into_iter().flatten().collect();
        }
        if failed {
            br_error!("wifi: off: no threads");
            return None; // a thread may be running: the session stays until close
        }
        Some(Box::new(WlanNet(wlan)) as Box<dyn NetBackend>)
    });
    Some((open, mac))
}

/// The stop: the network left, the supplicant as it was found, wlan0's settings back.
pub fn close() {
    if let Some(s) = lock(&THE_WLAN).take() {
        close_session(s);
    }
    // One UI's Wi-Fi off: the chip this session started goes down again, as its switch had it.
    super::chip::give_back();
}

fn close_session(s: Session) {
    let w = &s.wlan;
    if !s.threads.is_empty() {
        w.running.store(false, Ordering::Release);
        // SAFETY: one byte to our wake pipe.
        let _ = unsafe { libc::write(w.wake.1.as_raw_fd(), c"".as_ptr().cast(), 1) };
        {
            let mut q = lock(&w.queue);
            q.clear(); // the stop goes first
            q.push_back(Ct::Quit);
            w.more.notify_one();
        }
        for t in s.threads {
            let _ = t.join(); // the control thread: the supplicant: the network left, what was added removed
        }
    }
    if let Some(path) = &w.state_path {
        let _ = restore(path);
    }
    let t = w.traffic.read();
    br_info!(
        "wifi: closed ({} frames to the guest ({} bytes), {} from it ({} bytes), {} cut from coalesced frames; dropped: {} with the guest's ring full, {} too long, {} receive errors; {} of the guest's refused, {} sends failed; {} waited for the send buffer: {} sent, {} dropped after the wait, {} dropped with the waiting list full)",
        t.rx_frames,
        t.rx_bytes,
        t.tx_frames,
        t.tx_bytes,
        t.rx_cut,
        t.rx_ring_full,
        t.rx_too_long,
        t.rx_errors,
        t.tx_refused,
        t.tx_failed,
        t.tx_waited,
        t.tx_wait_sent,
        t.tx_wait_dropped,
        t.tx_held_full
    );
    // the Wlan stays while the device holds its backend: a vCPU may still be inside send (the process ends next)
}

/// One scan on the interface, for the tool: `each` gets each network. The number of networks.
pub fn scan_once(ifname: &str, wait_ms: u32, each: impl FnMut(&OwBss, &[u8])) -> Result<usize, i32> {
    let (ifname, ifindex, _, mut nl, events) = open_interface(ifname).ok_or(-libc::ENODEV)?;
    match trigger_scan(&mut nl, ifindex, 0) {
        Err(rc) if rc != -libc::EBUSY => {
            br_error!(
                "wifi: nl80211 TRIGGER_SCAN on {ifname}: {} (the networks the driver has are listed)",
                strerror(rc)
            )
        }
        _ => {
            let mut buffer = vec![0u8; 8192];
            let until = Instant::now() + Duration::from_millis(u64::from(wait_ms));
            let mut done = false;
            while !done && Instant::now() < until {
                if poll(&[events.as_raw_fd()], 250).map_or(true, |r| r[0] == 0) {
                    continue;
                }
                let Ok(got) = recv(events.as_raw_fd(), &mut buffer, libc::MSG_DONTWAIT) else { continue };
                done = messages(&buffer[..got]).any(|(kind, _, _, m)| {
                    m.len() >= NLMSG_HDRLEN + GENL_HDRLEN
                        && kind == nl.family
                        && matches!(m[NLMSG_HDRLEN], CMD_NEW_SCAN_RESULTS | CMD_SCAN_ABORTED)
                });
            }
        }
    }
    dump_scan(&mut nl, ifindex, each)
}

// ---- every radio, and Wonder's session (wonder, `wifictl wonder`) -------------------------------
fn dump_radios(nl: &mut Nl80211) -> Result<Radios, i32> {
    let mut out = Radios::default();
    let mut split = nl::Writer::new(8);
    split.flag(ATTR_SPLIT_WIPHY_DUMP);
    nl.call(CMD_GET_WIPHY, NLM_F_DUMP, split.finish().unwrap_or_default(), |attrs| out.wiphy(attrs))?; // no wiphy named: every one
    nl.call(CMD_GET_INTERFACE, NLM_F_DUMP, &[], |attrs| out.iface(attrs))?;
    Ok(out)
}

/// Every radio and interface (a split GET_WIPHY dump of every wiphy, a GET_INTERFACE dump). Read only.
pub fn radios() -> Result<Radios, i32> {
    let (mut nl, _) = Nl80211::open().map_err(|_| -libc::ENODEV)?;
    dump_radios(&mut nl)
}

/// The phone's WonderOps: nl80211, SIOCSIFFLAGS, AF_PACKET.
pub struct PhoneWonder {
    nl: Nl80211,
    frames: Vec<OwnedFd>,
}

impl PhoneWonder {
    /// None: nl80211 not opened.
    pub fn open() -> Option<PhoneWonder> {
        Nl80211::open().ok().map(|(nl, _)| PhoneWonder { nl, frames: Vec::new() })
    }
}

impl WonderOps for PhoneWonder {
    fn radios(&mut self) -> Result<Radios, i32> {
        dump_radios(&mut self.nl)
    }

    /// The wiphy's own regulatory domain (a self-managed one), else the kernel's: both answer GET_REG with the wiphy
    /// named.
    fn country(&mut self, wiphy: u32) -> Result<String, i32> {
        let mut cc = String::new();
        let get = |nl: &mut Nl80211, attrs: &[u8], cc: &mut String| {
            nl.call(CMD_GET_REG, 0, attrs, |a| {
                if let Some((_, d)) = nl::attrs(a).find(|(t, d)| *t == ATTR_REG_ALPHA2 && d.len() >= 2) {
                    *cc = String::from_utf8_lossy(&d[..2]).into_owned();
                }
            })
        };
        let mut w = nl::Writer::new(8);
        w.u32(ATTR_WIPHY, wiphy);
        let mut rc = get(&mut self.nl, w.finish().unwrap_or_default(), &mut cc);
        if rc.is_err() || cc.is_empty() {
            rc = get(&mut self.nl, &[], &mut cc);
        }
        rc?;
        if cc.is_empty() {
            Err(-libc::ENOENT)
        } else {
            Ok(cc)
        }
    }

    fn new_monitor(&mut self, wiphy: u32, name: &str) -> Result<u32, i32> {
        let mut w = nl::Writer::new(64);
        w.u32(ATTR_WIPHY, wiphy).attr(ATTR_IFNAME, format!("{name}\0").as_bytes()).u32(ATTR_IFTYPE, IFTYPE_MONITOR);
        let mut index = 0;
        self.nl.call(CMD_NEW_INTERFACE, NLM_F_ACK, w.finish().unwrap_or_default(), |a| {
            if let Some(i) =
                nl::attrs(a).find(|(t, d)| *t == ATTR_IFINDEX && d.len() == 4).and_then(|(_, d)| nl::u32_of(d))
            {
                index = i;
            }
        })?;
        if index == 0 {
            // SAFETY: a NUL-terminated name.
            index = CString::new(name).map_or(0, |c| unsafe { libc::if_nametoindex(c.as_ptr()) });
        }
        if index == 0 {
            Err(-libc::ENODEV)
        } else {
            Ok(index)
        }
    }

    fn del_iface(&mut self, ifindex: u32) -> Result<(), i32> {
        self.nl.call(CMD_DEL_INTERFACE, NLM_F_ACK, &ifindex_attrs(ifindex), |_| {})
    }

    fn set_up(&mut self, name: &str, up: bool) -> Result<(), i32> {
        let mut r = ifreq(name);
        ioctl_ifreq(SIOCGIFFLAGS, &mut r)?;
        let mut flags = i16::from_ne_bytes([r[16], r[17]]);
        let iff_up = libc::IFF_UP as i16;
        flags = if up { flags | iff_up } else { flags & !iff_up };
        r[16..18].copy_from_slice(&flags.to_ne_bytes());
        ioctl_ifreq(SIOCSIFFLAGS, &mut r) // up: mac80211's start (Wonder's), whose error is this one
    }

    fn vendor(&mut self, ifindex: u32, subcmd: u32, data: &[u8]) -> Result<Vec<u8>, i32> {
        if data.len() > 256 - 48 {
            return Err(-libc::E2BIG);
        }
        let mut w = nl::Writer::new(256);
        w.u32(ATTR_IFINDEX, ifindex).u32(ATTR_VENDOR_ID, OUI_GOOGLE).u32(ATTR_VENDOR_SUBCMD, subcmd);
        if !data.is_empty() {
            w.attr(ATTR_VENDOR_DATA, data); // raw: Wonder parses it
        }
        let mut reply = Vec::new();
        self.nl.call(CMD_VENDOR, NLM_F_ACK, w.finish().unwrap_or_default(), |a| {
            if let Some((_, d)) = nl::attrs(a).find(|(t, _)| *t == ATTR_VENDOR_DATA) {
                reply = d[..d.len().min(64)].to_vec();
            }
        })?;
        Ok(reply)
    }

    fn open_frames(&mut self, ifindex: u32) -> Result<i32, i32> {
        let fd = socket(libc::AF_PACKET, libc::SOCK_RAW, i32::from(ETH_P_ALL.to_be()))?;
        bind_packet(&fd, ifindex)?;
        let h = fd.as_raw_fd();
        self.frames.push(fd);
        Ok(h)
    }

    fn receive(&mut self, handle: i32, buffer: &mut [u8], wait_ms: u32) -> Result<usize, i32> {
        if poll(&[handle], wait_ms as i32)?[0] == 0 {
            return Ok(0);
        }
        recv(handle, buffer, libc::MSG_DONTWAIT)
    }

    fn close_frames(&mut self, handle: i32) {
        self.frames.retain(|fd| fd.as_raw_fd() != handle);
    }

    /// Associated: GET_STATION on a station gives its access point; the channel from GET_INTERFACE.
    fn link(&mut self, ifname: &str) -> Result<Link, i32> {
        let (index, _) = interface(ifname).ok_or(-libc::ENODEV)?;
        let attrs = ifindex_attrs(index);
        let mut l = Link::default();
        self.nl.call(CMD_GET_STATION, NLM_F_DUMP, &attrs, |a| station_attrs(a, &mut l))?;
        l.up = l.bssid != [0; 6];
        let _ = self.nl.call(CMD_GET_INTERFACE, 0, &attrs, |a| interface_freq(a, &mut l));
        Ok(l)
    }

    fn now_ms(&mut self) -> u64 {
        xh_core::time::monotonic_ms()
    }

    fn pause_ms(&mut self, ms: u32) {
        std::thread::sleep(Duration::from_millis(u64::from(ms)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// nl80211's, netlink's and AF_PACKET's numbers above against the build host's headers.
    #[test]
    fn numbers_match_the_c_headers() {
        let v = |x: u16| i64::from(x);
        let c = |x: u8| i64::from(x);
        #[rustfmt::skip]
        let pairs: &[(&str, i64)] = &[
            ("NETLINK_GENERIC", NETLINK_GENERIC.into()), ("NLM_F_REQUEST", v(NLM_F_REQUEST)), ("NLM_F_MULTI", v(NLM_F_MULTI)),
            ("NLM_F_ACK", v(NLM_F_ACK)), ("NLM_F_DUMP", v(NLM_F_DUMP)), ("NLMSG_ERROR", v(NLMSG_ERROR)), ("NLMSG_DONE", v(NLMSG_DONE)),
            ("NLMSG_HDRLEN", NLMSG_HDRLEN as i64), ("GENL_HDRLEN", GENL_HDRLEN as i64), ("GENL_ID_CTRL", v(GENL_ID_CTRL)),
            ("CTRL_CMD_GETFAMILY", c(CTRL_CMD_GETFAMILY)), ("CTRL_ATTR_FAMILY_ID", v(CTRL_ATTR_FAMILY_ID)),
            ("CTRL_ATTR_FAMILY_NAME", v(CTRL_ATTR_FAMILY_NAME)), ("CTRL_ATTR_MCAST_GROUPS", v(CTRL_ATTR_MCAST_GROUPS)),
            ("SOL_NETLINK", SOL_NETLINK.into()), ("NETLINK_ADD_MEMBERSHIP", NETLINK_ADD_MEMBERSHIP.into()),
            ("NL80211_CMD_GET_WIPHY", c(CMD_GET_WIPHY)), ("NL80211_CMD_GET_INTERFACE", c(CMD_GET_INTERFACE)),
            ("NL80211_CMD_NEW_INTERFACE", c(CMD_NEW_INTERFACE)), ("NL80211_CMD_DEL_INTERFACE", c(CMD_DEL_INTERFACE)),
            ("NL80211_CMD_GET_STATION", c(CMD_GET_STATION)), ("NL80211_CMD_GET_REG", c(CMD_GET_REG)),
            ("NL80211_CMD_GET_SCAN", c(CMD_GET_SCAN)), ("NL80211_CMD_TRIGGER_SCAN", c(CMD_TRIGGER_SCAN)),
            ("NL80211_CMD_NEW_SCAN_RESULTS", c(CMD_NEW_SCAN_RESULTS)), ("NL80211_CMD_SCAN_ABORTED", c(CMD_SCAN_ABORTED)),
            ("NL80211_CMD_VENDOR", c(CMD_VENDOR)),
            ("NL80211_ATTR_WIPHY", v(ATTR_WIPHY)), ("NL80211_ATTR_IFINDEX", v(ATTR_IFINDEX)), ("NL80211_ATTR_IFNAME", v(ATTR_IFNAME)),
            ("NL80211_ATTR_IFTYPE", v(ATTR_IFTYPE)), ("NL80211_ATTR_MAC", v(ATTR_MAC)), ("NL80211_ATTR_STA_INFO", v(ATTR_STA_INFO)),
            ("NL80211_ATTR_WIPHY_BANDS", v(ATTR_WIPHY_BANDS)), ("NL80211_ATTR_REG_ALPHA2", v(ATTR_REG_ALPHA2)),
            ("NL80211_ATTR_WIPHY_FREQ", v(ATTR_WIPHY_FREQ)), ("NL80211_ATTR_SCAN_SSIDS", v(ATTR_SCAN_SSIDS)),
            ("NL80211_ATTR_SCAN_FLAGS", v(ATTR_SCAN_FLAGS)), ("NL80211_SCAN_FLAG_LOW_PRIORITY", SCAN_FLAG_LOW_PRIORITY.into()),
            ("NL80211_SCAN_FLAG_LOW_SPAN", SCAN_FLAG_LOW_SPAN.into()),
            ("NL80211_ATTR_BSS", v(ATTR_BSS)), ("NL80211_ATTR_SPLIT_WIPHY_DUMP", v(ATTR_SPLIT_WIPHY_DUMP)),
            ("NL80211_ATTR_VENDOR_ID", v(ATTR_VENDOR_ID)), ("NL80211_ATTR_VENDOR_SUBCMD", v(ATTR_VENDOR_SUBCMD)),
            ("NL80211_ATTR_VENDOR_DATA", v(ATTR_VENDOR_DATA)),
            ("NL80211_BSS_BSSID", v(BSS_BSSID)), ("NL80211_BSS_FREQUENCY", v(BSS_FREQUENCY)),
            ("NL80211_BSS_BEACON_INTERVAL", v(BSS_BEACON_INTERVAL)), ("NL80211_BSS_CAPABILITY", v(BSS_CAPABILITY)),
            ("NL80211_BSS_INFORMATION_ELEMENTS", v(BSS_INFORMATION_ELEMENTS)), ("NL80211_BSS_SIGNAL_MBM", v(BSS_SIGNAL_MBM)),
            ("NL80211_BSS_SEEN_MS_AGO", v(BSS_SEEN_MS_AGO)), ("NL80211_STA_INFO_SIGNAL", v(STA_INFO_SIGNAL)),
            ("NL80211_STA_INFO_TX_BITRATE", v(STA_INFO_TX_BITRATE)), ("NL80211_RATE_INFO_BITRATE", v(RATE_INFO_BITRATE)),
            ("NL80211_RATE_INFO_BITRATE32", v(RATE_INFO_BITRATE32)), ("NL80211_IFTYPE_MONITOR", IFTYPE_MONITOR.into()),
            ("NL80211_BAND_ATTR_FREQS", 1), ("NL80211_FREQUENCY_ATTR_FREQ", 1), ("NL80211_FREQUENCY_ATTR_DISABLED", 2),
            ("NL80211_FREQUENCY_ATTR_NO_IR", 3), ("NL80211_FREQUENCY_ATTR_RADAR", 5), ("NL80211_FREQUENCY_ATTR_NO_HT40_MINUS", 9),
            ("NL80211_FREQUENCY_ATTR_NO_HT40_PLUS", 10), ("NL80211_FREQUENCY_ATTR_NO_80MHZ", 11),
            ("NL80211_FREQUENCY_ATTR_INDOOR_ONLY", 14), ("NL80211_ATTR_SUPPORTED_IFTYPES", 32),
            ("NL80211_ATTR_SUPPORTED_COMMANDS", 50), ("NL80211_ATTR_TX_FRAME_TYPES", 99), ("NL80211_ATTR_RX_FRAME_TYPES", 100),
            ("NL80211_ATTR_FRAME_TYPE", 101), ("NL80211_ATTR_OFFCHANNEL_TX_OK", 108),
            ("NL80211_ATTR_MAX_REMAIN_ON_CHANNEL_DURATION", 111), ("NL80211_ATTR_INTERFACE_COMBINATIONS", 120),
            ("NL80211_ATTR_SOFTWARE_IFTYPES", 121), ("NL80211_ATTR_FEATURE_FLAGS", 143), ("NL80211_ATTR_VENDOR_EVENTS", 198),
            ("NL80211_ATTR_EXT_FEATURES", 217), ("NL80211_CMD_REMAIN_ON_CHANNEL", 55), ("NL80211_CMD_FRAME", 59),
            ("NL80211_CMD_START_P2P_DEVICE", 89), ("NL80211_CMD_START_NAN", 115), ("NL80211_FEATURE_ACTIVE_MONITOR", 1 << 17),
            ("NL80211_EXT_FEATURE_SECURE_NAN", 63), // NUM_NL80211_IFTYPES: 13 in the NDK r28c header p2p.rs follows, more in newer hosts' headers
            ("SOL_PACKET", SOL_PACKET.into()), ("PACKET_ADD_MEMBERSHIP", PACKET_ADD_MEMBERSHIP.into()),
            ("PACKET_VNET_HDR", PACKET_VNET_HDR.into()), ("PACKET_IGNORE_OUTGOING", PACKET_IGNORE_OUTGOING.into()),
            ("PACKET_MR_ALLMULTI", v(PACKET_MR_ALLMULTI)), ("ETH_P_ALL", v(ETH_P_ALL)),
            ("sizeof(struct virtio_net_hdr)", VNET_HDR as i64), ("VIRTIO_NET_HDR_F_NEEDS_CSUM", c(VIRTIO_NET_HDR_F_NEEDS_CSUM)),
            ("VIRTIO_NET_HDR_GSO_NONE", c(VIRTIO_NET_HDR_GSO_NONE)), ("SIOCETHTOOL", SIOCETHTOOL as i64),
            ("SIOCGIFFLAGS", SIOCGIFFLAGS as i64), ("SIOCSIFFLAGS", SIOCSIFFLAGS as i64), ("ETHTOOL_GGRO", ETHTOOL_GGRO.into()),
            ("ETHTOOL_SGRO", ETHTOOL_SGRO.into()), ("sizeof(struct ifreq)", 40), ("sizeof(struct packet_mreq)", 16),
        ];
        crate::hal::test_c::check(
            "nl80211",
            &[
                "linux/netlink.h",
                "linux/genetlink.h",
                "linux/nl80211.h",
                "linux/if_packet.h",
                "linux/if_ether.h",
                "linux/virtio_net.h",
                "linux/ethtool.h",
                "linux/sockios.h",
                "net/if.h",
            ],
            pairs,
        );
    }

    /// A scan while joined tries low priority and low span, then low priority, then none, as the driver refuses.
    #[test]
    fn joined_scan_flags_fall_back() {
        assert_eq!(JOINED_SCAN_FLAGS, [0x101, 0x1, 0]);
        assert_eq!(joined_scan_next(0, Err(-libc::EOPNOTSUPP)), Some(1));
        assert_eq!(joined_scan_next(1, Err(-libc::EINVAL)), Some(2));
        assert_eq!(joined_scan_next(2, Err(-libc::EOPNOTSUPP)), None); // nothing left to drop
        assert_eq!(joined_scan_next(0, Ok(())), None);
        assert_eq!(joined_scan_next(0, Err(-libc::EBUSY)), None); // busy is an answer, not a refusal of the flags
    }

    #[test]
    fn netlink_messages_and_scan_records() {
        // Two messages in a datagram, the second cut.
        let mut d = Vec::new();
        for (seq, kind) in [(7u32, NLMSG_DONE), (8, 0x1c)] {
            d.extend_from_slice(&20u32.to_ne_bytes());
            d.extend_from_slice(&kind.to_ne_bytes());
            d.extend_from_slice(&NLM_F_MULTI.to_ne_bytes());
            d.extend_from_slice(&seq.to_ne_bytes());
            d.extend_from_slice(&[0; 8]);
        }
        let got: Vec<(u16, u32)> = messages(&d).map(|(k, _, s, _)| (k, s)).collect();
        assert_eq!(got, vec![(NLMSG_DONE, 7), (0x1c, 8)]);
        assert_eq!(messages(&d[..30]).count(), 1);
        // A BSS as GET_SCAN gives it: the name from the SSID element.
        let mut w = nl::Writer::new(256);
        w.attr(BSS_BSSID, &[1, 2, 3, 4, 5, 6])
            .u32(BSS_FREQUENCY, 5180)
            .attr(BSS_CAPABILITY, &0x411u16.to_le_bytes())
            .u32(BSS_SIGNAL_MBM, (-5500i32) as u32)
            .attr(BSS_INFORMATION_ELEMENTS, b"\x00\x04home\x30\x02\x01\x00");
        let b = bss_of(w.finish().unwrap()).unwrap();
        assert!(b.bss.ssid() == b"home" && b.bss.signal_dbm == -55 && b.bss.freq_mhz == 5180 && b.ies.len() == 10);
        assert!(bss_of(nl::Writer::new(16).u32(BSS_FREQUENCY, 1).finish().unwrap()).is_none());
        // A station entry: signal and the 32-bit rate before the 16-bit one.
        let mut rate = nl::Writer::new(32);
        rate.attr(RATE_INFO_BITRATE, &4330u16.to_le_bytes()).u32(RATE_INFO_BITRATE32, 8660);
        let mut info = nl::Writer::new(64);
        info.attr(STA_INFO_SIGNAL, &[(-48i8) as u8]).attr(STA_INFO_TX_BITRATE, rate.finish().unwrap());
        let mut sta = nl::Writer::new(128);
        sta.attr(ATTR_MAC, &[9; 6]).attr(ATTR_STA_INFO, info.finish().unwrap());
        let mut l = Link::default();
        station_attrs(sta.finish().unwrap(), &mut l);
        assert!(l.bssid == [9; 6] && l.rssi_dbm == -48 && l.tx_rate_mbps == 866);
    }

    #[test]
    fn state_file() {
        let dir = std::env::temp_dir().join(format!("onebridges-wlan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("wifi.state");
        let path = p.to_str().unwrap();
        assert_eq!(restore(path), Err(-libc::ENOENT));
        std::fs::write(&p, "ifname=wlan0\ndisable_ipv6=1\ngro=0\n").unwrap();
        assert_eq!(read_state(path, 0, 1), Ok(("wlan0".into(), 1, 0)));
        std::fs::write(&p, "ifname=../x\n").unwrap();
        assert_eq!(read_state(path, 0, 1), Err(false));
        assert_eq!(restore(path), Err(-libc::EINVAL));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
