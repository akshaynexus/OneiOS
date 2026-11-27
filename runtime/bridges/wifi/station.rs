// SPDX-License-Identifier: MIT
//! The station the guest's Wi-Fi driver (kernel/patches/oneios-hosted/net/OneiOSWifi.cpp) talks to: its messages
//! ([`super::proto`]) become calls on a radio back end, and what the radio reports becomes messages for the guest. No
//! Linux interface here: the phone's back end is [`super::wlan`], a boot with no phone [`super::sim`], the tests a
//! fake radio. C: wifi/station.c.

use super::proto::*;
use crate::proto::wire::Wire;
use crate::{br_error, br_info, br_log};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The networks of the last scan kept (also the join's source of what a network offers).
const SCAN_KEEP: usize = 64;
/// The elements of a network sent to the guest at most (whole ones).
const SENT_IES: usize = 1024;
const _: () = assert!(SENT_IES <= OW_MAX_IES, "the elements of a network fit a message");

/// What the radio back end reports about the link.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Link {
    pub up: bool,
    pub rssi_dbm: i32,
    pub noise_dbm: i32,
    pub tx_rate_mbps: u32,
    pub freq_mhz: u32,
    pub reason: u32,
    pub bssid: [u8; 6],
}

/// A network of a scan: its record and its elements.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bss {
    pub bss: OwBss,
    pub ies: Vec<u8>,
}

/// The radio, as the station uses it. Each call returns at once; scan and join answer later through
/// [`Station::scan_add`]/[`Station::scan_done`] and [`Station::join_done`] (on the station each call is given, or
/// from the back end's own thread). Errors are negative errnos.
pub trait Radio: Send + Sync {
    fn scan(&self, st: &Station) -> Result<(), i32>;
    /// `bss`: the last scan's record of the network, or None.
    fn join(&self, st: &Station, join: &OwJoin, bss: Option<&Bss>) -> Result<(), i32>;
    fn leave(&self, st: &Station) -> Result<(), i32>;
    fn link(&self) -> Result<Link, i32>;
    /// The channels it may use (None: it does not say).
    fn channels(&self, max: usize) -> Option<Result<Vec<OwChannel>, i32>> {
        let _ = max;
        None
    }
    /// While joined, how long after a scan its networks answer the next scan instead of the radio (None: never; each
    /// scan is the radio's). A scan of every channel takes a joined radio off the network's channel channel by
    /// channel: a real driver answers a scan soon after the last one from its cache.
    fn joined_scan_reuse(&self) -> Option<Duration> {
        None
    }
}

/// Each frame for the guest (the station's messages, as frames of the control type).
pub type GuestFn = Box<dyn Fn(&[u8]) + Send + Sync>;

#[derive(Default)]
struct Inner {
    scan: Vec<Bss>,
    scanning: bool,
    joining: bool,
    joined: bool,
    current: Bss,
    last_reason: u32,
    country: [u8; 2],
    last_scan: Option<(Instant, Vec<Bss>)>, // the last good scan of the radio's: when it ended, its networks
}

pub struct Station {
    radio: Box<dyn Radio>,
    mac: [u8; 6],
    to_guest: GuestFn,
    inner: Mutex<Inner>,
    cached: AtomicU64, // scans answered from the last scan's networks
}

impl Station {
    /// `country`: the regulatory country, "00" when unknown (a scan's strongest Country element sets it).
    pub fn new(radio: Box<dyn Radio>, mac: [u8; 6], country: Option<[u8; 2]>, to_guest: GuestFn) -> Station {
        let country = country.filter(|c| c[0] != 0).unwrap_or(*b"00");
        Station {
            radio,
            mac,
            to_guest,
            inner: Mutex::new(Inner { country, ..Default::default() }),
            cached: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn radio(&self) -> &dyn Radio {
        &*self.radio
    }

    /// The radio is associated before the guest asks: the station's first message reports the link up with `bss`, as
    /// if the join had happened (the tap's made-up network, `wifi=cell`; docs/hardware.md 3.4b).
    pub fn assume_joined(&self, bss: Bss) {
        let mut i = self.lock();
        i.joined = true;
        i.current = bss;
    }

    fn send(&self, kind: u16, parts: &[&[u8]]) {
        // to the guest: its own address passes its receive filter
        if let Some(f) = frame(&self.mac, &OW_ADDRESS, kind, parts) {
            (self.to_guest)(&f);
        }
    }

    /// The state message: the radio's view of the link, the joined network's record and elements.
    fn state(&self) -> (OwState, Vec<u8>) {
        let mut st = OwState { version: OW_VERSION, radio: OW_RADIO_OK, mac: self.mac, ..Default::default() };
        let link = self.radio.link();
        let mut i = self.lock();
        st.country = i.country;
        match link {
            Ok(l) if l.up && i.joined => {
                st.link = OW_LINK_UP;
                st.rssi_dbm = l.rssi_dbm;
                st.noise_dbm = l.noise_dbm;
                st.tx_rate_mbps = l.tx_rate_mbps;
                if l.freq_mhz != 0 {
                    i.current.bss.freq_mhz = l.freq_mhz;
                }
                i.current.bss.signal_dbm = l.rssi_dbm as i16;
                if l.bssid != [0; 6] {
                    i.current.bss.bssid = l.bssid;
                }
                st.bss = i.current.bss;
                let n = (st.bss.ie_length as usize).min(i.current.ies.len());
                (st, i.current.ies[..n].to_vec())
            }
            _ => {
                if let (Ok(l), true) = (link, i.joined) {
                    if !l.up {
                        i.joined = false;
                        i.last_reason = l.reason;
                    }
                }
                st.link = if i.joining {
                    OW_LINK_AUTH
                } else if i.scanning {
                    OW_LINK_SCANNING
                } else {
                    OW_LINK_IDLE
                };
                st.reason = i.last_reason;
                (st, Vec::new())
            }
        }
    }

    /// What the station sends for a state (for the tests and the tool).
    pub fn state_frame(&self) -> Option<Vec<u8>> {
        let (st, ies) = self.state();
        frame(&self.mac, &OW_ADDRESS, OW_STATE, &[&st.to_bytes(), &ies])
    }

    /// The link changed (or a periodic look): the guest gets OW_STATE.
    pub fn link_changed(&self) {
        if let Some(f) = self.state_frame() {
            (self.to_guest)(&f);
        }
    }

    /// One network of a scan: whole elements only, as many as iOS's scan record holds.
    pub fn scan_add(&self, bss: &OwBss, ies: &[u8]) {
        let mut whole = 0;
        while whole + 2 <= ies.len() {
            let next = whole + 2 + ies[whole + 1] as usize;
            if next > ies.len() || next > SENT_IES {
                break;
            }
            whole = next;
        }
        let ies = &ies[..whole];
        let record = OwBss { ie_length: whole as u16, ..*bss };
        let keep = {
            let mut i = self.lock();
            let keep = i.scan.len() < SCAN_KEEP;
            if keep {
                i.scan.push(Bss { bss: record, ies: ies.to_vec() });
            }
            keep
        };
        if keep {
            self.send(OW_BSS, &[&record.to_bytes(), ies]);
        }
    }

    /// The radio's scan ended (`error`: 0, or a negative errno).
    pub fn scan_done(&self, error: i32) {
        self.finish_scan(error, false);
    }

    /// Scans answered from the last scan's networks so far (the 60 s line).
    pub fn cached_scans(&self) -> u64 {
        self.cached.load(Ordering::Relaxed)
    }

    /// A scan's end: the radio's (a good one's networks are kept for [`Radio::joined_scan_reuse`]) or a cached
    /// answer's (`cached`: the kept networks and their time stay).
    fn finish_scan(&self, error: i32, cached: bool) {
        let (done, changed) = {
            let mut i = self.lock();
            let done = OwScanDone { count: i.scan.len() as u32, error };
            i.scanning = false;
            if error == 0 && !cached && !i.scan.is_empty() {
                i.last_scan = Some((Instant::now(), i.scan.clone()));
            }
            let country = scan_country(&i.scan).filter(|c| *c != i.country);
            if let Some(c) = country {
                i.country = c;
            }
            (done, country.is_some())
        };
        if changed {
            self.link_changed(); // the state carries the country
        }
        self.send(OW_SCAN_DONE, &[&done.to_bytes()]);
    }

    /// A join's result (a result nobody waits for changes nothing).
    pub fn join_done(&self, status: u32, code: u32) {
        {
            let mut i = self.lock();
            if !i.joining {
                return;
            }
            i.joining = false;
            i.joined = status == OW_JOIN_OK;
            if !i.joined {
                i.last_reason = code;
            }
        }
        if status == OW_JOIN_OK {
            self.link_changed(); // the network's record first: the driver copies it on the result
        }
        self.send(OW_JOIN_RESULT, &[&OwJoinResult { status, code }.to_bytes()]);
        br_log!(
            if status == OW_JOIN_OK { crate::log::Level::Info } else { crate::log::Level::Error },
            "wifi: join {} (status {status}, code {code})",
            if status == OW_JOIN_OK { "done" } else { "failed" }
        );
    }

    /// A control frame from the guest (ethertype == CONTROL_TYPE).
    pub fn message(&self, frame: &[u8]) {
        let Some((h, p)) = message(frame) else { return };
        match h.kind {
            OW_HELLO => {
                br_info!("wifi: the guest's driver is up; it gets the radio");
                self.link_changed();
                // the channels the phone may use: APPLE80211_IOC_CHANNELS_INFO and the channel lists
                match self.radio.channels(OW_MAX_CHANNELS) {
                    None => {}
                    Some(Ok(ch)) if !ch.is_empty() => {
                        let head = OwChannels { count: ch.len() as u16, pad: 0 };
                        let list: Vec<u8> = ch.iter().flat_map(|c| c.to_bytes()).collect();
                        self.send(OW_CHANNELS, &[&head.to_bytes(), &list]);
                        br_info!(
                            "wifi: {} channels of the radio for the guest ({} MHz to {} MHz)",
                            ch.len(),
                            ch[0].freq_mhz,
                            ch[ch.len() - 1].freq_mhz
                        );
                    }
                    Some(r) => br_error!(
                        "wifi: the radio's channel list: {}; the guest keeps its own",
                        match r {
                            Err(rc) => crate::hal::binder::strerror(rc),
                            Ok(_) => "empty".to_owned(),
                        }
                    ),
                }
            }
            OW_SCAN => {
                let reuse = self.radio.joined_scan_reuse();
                let (start, cached) = {
                    let mut i = self.lock();
                    let start = !i.scanning;
                    let mut cached = None;
                    if start {
                        i.scanning = true;
                        i.scan.clear();
                        cached =
                            cached_answer(i.joined, i.last_scan.as_ref(), reuse, Instant::now()).map(<[Bss]>::to_vec);
                    }
                    (start, cached)
                };
                if let Some(networks) = cached {
                    // while joined, soon after the last scan: its networks, and the radio stays on its channel
                    self.cached.fetch_add(1, Ordering::Relaxed);
                    for b in &networks {
                        self.scan_add(&b.bss, &b.ies);
                    }
                    self.finish_scan(0, true);
                } else if start && self.radio.scan(self).is_err() {
                    self.scan_done(-libc::EIO);
                }
            }
            OW_JOIN => {
                let Some(mut j) = OwJoin::parse(p) else { return };
                if j.ssid_length == 0 || j.ssid_length > 32 {
                    return;
                }
                let known = {
                    let mut i = self.lock();
                    let found = i
                        .scan
                        .iter()
                        .filter(|b| b.bss.ssid() == j.ssid())
                        .fold(None::<&Bss>, |best, b| match best {
                            Some(x) if x.bss.signal_dbm >= b.bss.signal_dbm => Some(x),
                            _ => Some(b),
                        })
                        .cloned();
                    i.current = found.clone().unwrap_or_else(|| {
                        let mut c = Bss::default();
                        c.bss.ssid_length = j.ssid_length;
                        c.bss.ssid[..j.ssid().len()].copy_from_slice(j.ssid());
                        c
                    });
                    i.joining = true;
                    i.joined = false;
                    found
                };
                let rc = self.radio.join(self, &j, known.as_ref());
                j.wipe();
                if let Err(rc) = rc {
                    self.join_done(if rc == -libc::ENOENT { OW_JOIN_NO_NETWORK } else { OW_JOIN_FAILED }, 0);
                }
            }
            // iOS's Wi-Fi switch: off leaves the network; the radio itself stays on
            OW_POWER if OwPower::parse(p).unwrap_or(OwPower { on: 1 }).on != 0 => {}
            OW_POWER | OW_LEAVE => {
                {
                    let mut i = self.lock();
                    i.joining = false;
                    i.joined = false;
                    i.last_reason = 3; // deauthenticated: the station is leaving
                }
                let _ = self.radio.leave(self);
                self.link_changed();
            }
            _ => {}
        }
    }
}

/// The networks that answer a scan instead of the radio: the last good scan's (`last`: when it ended, its
/// networks), while `joined`, within the radio's `reuse` of its end (None: the radio scans each time).
pub(crate) fn cached_answer(
    joined: bool,
    last: Option<&(Instant, Vec<Bss>)>,
    reuse: Option<Duration>,
    now: Instant,
) -> Option<&[Bss]> {
    let (at, networks) = last?;
    (joined && now.saturating_duration_since(*at) < reuse?).then_some(&networks[..])
}

/// The regulatory country: the Country element (7) of the strongest network that has one, as iOS's own drivers
/// take it.
fn scan_country(scan: &[Bss]) -> Option<[u8; 2]> {
    let mut best = i32::from(i16::MIN);
    let mut found = None;
    for b in scan {
        if i32::from(b.bss.signal_dbm) <= best {
            continue;
        }
        let ies = &b.ies[..(b.bss.ie_length as usize).min(b.ies.len())];
        let mut o = 0;
        while o + 2 <= ies.len() && o + 2 + ies[o + 1] as usize <= ies.len() {
            if ies[o] == 7 && ies[o + 1] >= 2 && ies[o + 2].is_ascii_uppercase() && ies[o + 3].is_ascii_uppercase() {
                found = Some([ies[o + 2], ies[o + 3]]);
                best = i32::from(b.bss.signal_dbm);
                break;
            }
            o += 2 + ies[o + 1] as usize;
        }
    }
    found
}

/// A frame of the guest's: dst OW_ADDRESS, src `mac` (the tests, wifictl's join).
pub fn guest_frame(mac: &[u8; 6], kind: u16, payload: &[u8]) -> Vec<u8> {
    frame(&OW_ADDRESS, mac, kind, &[payload]).unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::frames::tests::MAC;
    use super::*;
    use std::sync::{Arc, Mutex};

    /// The frames the guest got.
    pub type Sink = Arc<Mutex<Vec<Vec<u8>>>>;
    pub fn sink() -> (Sink, GuestFn) {
        let s: Sink = Arc::default();
        let t = s.clone();
        (s, Box::new(move |f: &[u8]| t.lock().unwrap().push(f.to_vec())))
    }

    /// The message of host frame i: its type and payload (test_wifi.c's host_message; 0: none).
    pub fn host_message(s: &Sink, i: usize) -> (u16, Vec<u8>) {
        let s = s.lock().unwrap();
        let Some(f) = s.get(i) else { return (0, Vec::new()) };
        if f.len() < 22 || f[..6] != MAC || f[6..12] != OW_ADDRESS {
            return (0, Vec::new());
        }
        let h = OwHeader::parse(&f[14..]).unwrap();
        if h.magic != OW_MAGIC || 22 + h.length as usize != f.len() {
            return (0, Vec::new());
        }
        (h.kind, f[22..].to_vec())
    }

    #[derive(Default)]
    struct FakeState {
        scans: u32,
        joins: u32,
        leaves: u32,
        join_rc: i32,
        join: OwJoin,
        bss: Option<Bss>,
        link: Link,
        reuse: Option<Duration>,
    }
    #[derive(Clone, Default)]
    struct FakeRadio(Arc<Mutex<FakeState>>);
    impl Radio for FakeRadio {
        fn scan(&self, _st: &Station) -> Result<(), i32> {
            self.0.lock().unwrap().scans += 1;
            Ok(())
        }
        fn join(&self, _st: &Station, j: &OwJoin, bss: Option<&Bss>) -> Result<(), i32> {
            let mut r = self.0.lock().unwrap();
            r.joins += 1;
            r.join = *j;
            r.bss = bss.cloned();
            if r.join_rc != 0 {
                Err(r.join_rc)
            } else {
                Ok(())
            }
        }
        fn leave(&self, _st: &Station) -> Result<(), i32> {
            self.0.lock().unwrap().leaves += 1;
            Ok(())
        }
        fn link(&self) -> Result<Link, i32> {
            Ok(self.0.lock().unwrap().link)
        }
        fn joined_scan_reuse(&self) -> Option<Duration> {
            self.0.lock().unwrap().reuse
        }
    }

    fn ssid32(s: &[u8]) -> [u8; 32] {
        let mut out = [0; 32];
        out[..s.len()].copy_from_slice(s);
        out
    }

    // test_wifi.c test_station: the station against a fake radio
    #[test]
    fn station() {
        let radio = FakeRadio::default();
        let r = radio.0.clone();
        let (s, to_guest) = sink();
        let sta = Station::new(Box::new(radio), MAC, None, to_guest);
        let msg = |kind, payload: &[u8]| sta.message(&guest_frame(&MAC, kind, payload));
        msg(OW_HELLO, &OwHello { version: OW_VERSION }.to_bytes());
        let (kind, p) = host_message(&s, 0);
        assert!(s.lock().unwrap().len() == 1 && kind == OW_STATE && p.len() == OwState::SIZE);
        let st = OwState::parse(&p).unwrap();
        assert!(
            st.version == OW_VERSION
                && st.radio == OW_RADIO_OK
                && st.link == OW_LINK_IDLE
                && st.mac == MAC
                && st.country == *b"00"
        );

        // A scan: two networks, the second seen twice (the stronger one is the join's), elements cut to whole ones.
        s.lock().unwrap().clear();
        msg(OW_SCAN, &[]);
        msg(OW_SCAN, &[]); // a second request while one runs: one scan
        assert!(r.lock().unwrap().scans == 1 && s.lock().unwrap().is_empty());
        let mut ies = vec![0u8; 1100];
        ies[..6].copy_from_slice(b"\x00\x04home"); // SSID
        ies[6..10].copy_from_slice(&[7, 20, b'I', b'N']); // Country (20 bytes; the RSN element of a real beacon comes after it)
        let mut at = 28;
        while at + 102 <= ies.len() {
            ies[at] = 221;
            ies[at + 1] = 100; // vendor elements up to 1048
            at += 102;
        }
        let mut b = OwBss {
            bssid: [1, 2, 3, 4, 5, 6],
            freq_mhz: 5180,
            signal_dbm: -70,
            ssid_length: 4,
            ssid: ssid32(b"home"),
            ..Default::default()
        };
        sta.scan_add(&b, &ies[..1048]);
        b.bssid[5] = 7;
        b.signal_dbm = -50;
        b.freq_mhz = 2437;
        sta.scan_add(&b, &ies[..28]);
        let other = OwBss {
            bssid: [9; 6],
            freq_mhz: 2412,
            signal_dbm: -80,
            ssid_length: 5,
            ssid: ssid32(b"other"),
            ..Default::default()
        };
        sta.scan_add(&other, &[]);
        sta.scan_done(0);
        // Five frames: three networks, the state with the country the strongest network's Country element names, the end.
        assert_eq!(s.lock().unwrap().len(), 5);
        let (kind, p) = host_message(&s, 3);
        assert!(kind == OW_STATE && OwState::parse(&p).unwrap().country == *b"IN");
        let (kind, p) = host_message(&s, 0);
        assert!(kind == OW_BSS && p.len() == OwBss::SIZE + 946); // 28 + 9 * 102: the tenth does not fit 1024
        let got = OwBss::parse(&p).unwrap();
        assert!(got.ie_length == 946 && got.freq_mhz == 5180 && p[OwBss::SIZE..] == ies[..946]);
        let (kind, p) = host_message(&s, 2);
        assert!(kind == OW_BSS && p.len() == OwBss::SIZE);
        let (kind, p) = host_message(&s, 4);
        assert!(kind == OW_SCAN_DONE && OwScanDone::parse(&p).unwrap() == OwScanDone { count: 3, error: 0 });

        // A join: the radio gets the request and the stronger record; the result follows the network's state.
        s.lock().unwrap().clear();
        let j = OwJoin::new(b"home", OW_SECURITY_AUTO, b"password", false);
        msg(OW_JOIN, &j.to_bytes());
        {
            let f = r.lock().unwrap();
            assert!(f.joins == 1 && f.bss.as_ref().is_some_and(|b| b.bss.bssid[5] == 7) && f.join.key() == b"password");
        }
        assert!(s.lock().unwrap().is_empty());
        r.lock().unwrap().link = Link {
            up: true,
            rssi_dbm: -48,
            noise_dbm: -95,
            tx_rate_mbps: 433,
            freq_mhz: 2437,
            bssid: [1, 2, 3, 4, 5, 7],
            reason: 0,
        };
        sta.join_done(OW_JOIN_OK, 0);
        assert_eq!(s.lock().unwrap().len(), 2);
        let (kind, p) = host_message(&s, 0);
        assert!(kind == OW_STATE && p.len() == OwState::SIZE + 28);
        let st = OwState::parse(&p).unwrap();
        assert!(st.link == OW_LINK_UP && st.rssi_dbm == -48 && st.tx_rate_mbps == 433 && st.bss.freq_mhz == 2437);
        assert!(st.bss.ssid_length == 4 && st.bss.ie_length == 28);
        let (kind, p) = host_message(&s, 1);
        assert!(kind == OW_JOIN_RESULT && OwJoinResult::parse(&p).unwrap().status == OW_JOIN_OK);
        sta.join_done(OW_JOIN_OK, 0); // a second result: none is pending
        assert_eq!(s.lock().unwrap().len(), 2);

        // The link drops: the state says idle and why.
        s.lock().unwrap().clear();
        r.lock().unwrap().link = Link { up: false, reason: 4, ..Default::default() };
        sta.link_changed();
        let (kind, p) = host_message(&s, 0);
        assert!(s.lock().unwrap().len() == 1 && kind == OW_STATE && p.len() == OwState::SIZE);
        let st = OwState::parse(&p).unwrap();
        assert!(st.link == OW_LINK_IDLE && st.reason == 4);

        // A network no scan saw; the radio refuses: the result says so.
        s.lock().unwrap().clear();
        let hidden = OwJoin::new(b"hidden", OW_SECURITY_OPEN, b"", false);
        r.lock().unwrap().join_rc = -libc::ENOENT;
        msg(OW_JOIN, &hidden.to_bytes());
        assert!(r.lock().unwrap().joins == 2 && r.lock().unwrap().bss.is_none() && s.lock().unwrap().len() == 1);
        let (kind, p) = host_message(&s, 0);
        assert!(kind == OW_JOIN_RESULT && OwJoinResult::parse(&p).unwrap().status == OW_JOIN_NO_NETWORK);
        // A failed join reports its code.
        s.lock().unwrap().clear();
        r.lock().unwrap().join_rc = 0;
        msg(OW_JOIN, &j.to_bytes());
        sta.join_done(OW_JOIN_AUTH, 15);
        let (kind, p) = host_message(&s, 0);
        assert!(s.lock().unwrap().len() == 1 && kind == OW_JOIN_RESULT);
        assert_eq!(OwJoinResult::parse(&p).unwrap(), OwJoinResult { status: OW_JOIN_AUTH, code: 15 });
        // Leave.
        s.lock().unwrap().clear();
        msg(OW_LEAVE, &[]);
        assert!(r.lock().unwrap().leaves == 1 && s.lock().unwrap().len() == 1 && host_message(&s, 0).0 == OW_STATE);
        // iOS's Wi-Fi switch: on changes nothing, off leaves the network.
        msg(OW_POWER, &OwPower { on: 1 }.to_bytes());
        assert!(r.lock().unwrap().leaves == 1 && s.lock().unwrap().len() == 1);
        msg(OW_POWER, &OwPower { on: 0 }.to_bytes());
        assert!(r.lock().unwrap().leaves == 2 && s.lock().unwrap().len() == 2 && host_message(&s, 1).0 == OW_STATE);
        // Malformed: a short frame, a wrong magic, a length past the frame, a join with no name.
        s.lock().unwrap().clear();
        let f = guest_frame(&MAC, OW_SCAN, &[]);
        sta.message(&f[..10]);
        let mut bad = f.clone();
        bad[14] ^= 1;
        sta.message(&bad);
        let f = guest_frame(&MAC, OW_JOIN, &j.to_bytes());
        sta.message(&f[..f.len() - 1]);
        msg(OW_JOIN, &OwJoin::default().to_bytes());
        let fr = r.lock().unwrap();
        assert!(s.lock().unwrap().is_empty() && fr.joins == 3 && fr.scans == 1 && fr.leaves == 2);
    }

    /// The networks that answer a scan instead of the radio: only while joined, only within the radio's reuse.
    #[test]
    fn cached_answer_rules() {
        let t0 = Instant::now();
        let nets = vec![Bss::default()];
        let last = Some((t0, nets));
        let reuse = Some(Duration::from_secs(120));
        let at = |s| t0 + Duration::from_secs(s);
        assert_eq!(cached_answer(true, last.as_ref(), reuse, at(119)).map(<[Bss]>::len), Some(1));
        assert!(cached_answer(true, last.as_ref(), reuse, at(120)).is_none()); // old: the radio scans
        assert!(cached_answer(false, last.as_ref(), reuse, at(1)).is_none()); // not joined: the radio scans
        assert!(cached_answer(true, last.as_ref(), None, at(1)).is_none()); // a radio that never reuses
        assert!(cached_answer(true, None, reuse, at(1)).is_none()); // no good scan yet
    }

    /// A scan while joined soon after the last one: its networks again and the scan's end, and the radio is not
    /// asked; the answer does not make the kept networks younger. Not joined: the radio scans.
    #[test]
    fn a_scan_while_joined_from_the_cache() {
        let radio = FakeRadio::default();
        let r = radio.0.clone();
        r.lock().unwrap().reuse = Some(Duration::from_secs(120));
        let (s, to_guest) = sink();
        let sta = Station::new(Box::new(radio), MAC, None, to_guest);
        let msg = |kind, payload: &[u8]| sta.message(&guest_frame(&MAC, kind, payload));
        let net = |n: u8, ssid: &[u8]| OwBss {
            bssid: [n; 6],
            freq_mhz: 5180,
            signal_dbm: -60,
            ssid_length: ssid.len() as u8,
            ssid: ssid32(ssid),
            ..Default::default()
        };
        // not joined: the radio's scan, kept
        msg(OW_SCAN, &[]);
        sta.scan_add(&net(1, b"home"), b"\x00\x04home");
        sta.scan_add(&net(2, b"other"), &[]);
        sta.scan_done(0);
        let kept = sta.lock().last_scan.as_ref().unwrap().0;
        msg(OW_SCAN, &[]);
        assert_eq!(r.lock().unwrap().scans, 2, "not joined: every scan is the radio's");
        sta.scan_add(&net(1, b"home"), b"\x00\x04home");
        sta.scan_add(&net(2, b"other"), &[]);
        sta.scan_done(0);
        let kept2 = sta.lock().last_scan.as_ref().unwrap().0;
        assert!(kept2 >= kept);
        // joined
        r.lock().unwrap().link = Link { up: true, rssi_dbm: -50, freq_mhz: 5180, bssid: [1; 6], ..Default::default() };
        msg(OW_JOIN, &OwJoin::new(b"home", OW_SECURITY_AUTO, b"password", false).to_bytes());
        sta.join_done(OW_JOIN_OK, 0);
        s.lock().unwrap().clear();
        for _ in 0..2 {
            msg(OW_SCAN, &[]);
            assert_eq!(s.lock().unwrap().len(), 3);
            let (k0, p0) = host_message(&s, 0);
            let (k1, p1) = host_message(&s, 1);
            let (k2, p2) = host_message(&s, 2);
            assert!(
                k0 == OW_BSS && OwBss::parse(&p0).unwrap().bssid == [1; 6] && p0[OwBss::SIZE..] == *b"\x00\x04home"
            );
            assert!(k1 == OW_BSS && OwBss::parse(&p1).unwrap().bssid == [2; 6] && p1.len() == OwBss::SIZE);
            assert!(k2 == OW_SCAN_DONE && OwScanDone::parse(&p2).unwrap() == OwScanDone { count: 2, error: 0 });
            s.lock().unwrap().clear();
        }
        assert_eq!((r.lock().unwrap().scans, sta.cached_scans()), (2, 2));
        {
            let i = sta.lock();
            assert!(!i.scanning && i.scan.len() == 2 && i.last_scan.as_ref().unwrap().0 == kept2);
        }
        // the kept scan grows old: the radio scans again
        sta.lock().last_scan.as_mut().unwrap().0 = Instant::now() - Duration::from_secs(121);
        msg(OW_SCAN, &[]);
        assert!(r.lock().unwrap().scans == 3 && s.lock().unwrap().is_empty());
        sta.scan_done(-libc::ECANCELED); // a failed scan is not kept
        assert!(sta.lock().last_scan.as_ref().unwrap().0 < kept2);
    }
}
