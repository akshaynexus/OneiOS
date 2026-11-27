// SPDX-License-Identifier: MIT
//! wifictl: the Wi-Fi bridge on the phone without xnuhost or a guest (docs/hardware.md section 3). Run as root (su):
//!   wifictl status [IFACE]           read only: the interface's address, the supplicant's AIDL service, its station
//!                                    interface, its networks' ids, the signal poll. Safe beside One UI.
//!   wifictl scan [SECONDS [IFACE]]   one nl80211 scan (default 6 s): each network's name, access point, channel
//!                                    frequency, signal and security. Safe beside One UI (its own scans do the same).
//!   wifictl join SSID [PASSPHRASE [SECONDS]]
//!                                    the whole host path with this tool as the guest: takes wlan0 as a session does
//!                                    (the supplicant's networks removed, IPv4 addresses removed), scans, joins, asks
//!                                    for an address by DHCP and prints the offer, stays SECONDS (default 5), leaves
//!                                    and gives wlan0 back. It takes the Wi-Fi from One UI while it runs: One UI joins
//!                                    its network again by itself after it. Not during a session.
//!   wifictl restore FILE             wlan0's settings back from a session's wifi.state (after a killed xnuhost)
//!   wifictl chip [start|stop|status]
//!                                    the Wi-Fi chip's HAL (android.hardware.wifi.IWifi/default), read and changed:
//!                                    status (default: its version, whether the chip runs, its station interfaces),
//!                                    start (IWifi.start, then wait for wlan0; for a session that starts with One UI's
//!                                    Wi-Fi off), stop (IWifi.stop: for every user, One UI's switch starts it again).
//!                                    Safe beside One UI; a session with no wlan0 does the start itself.
//!   wifictl p2p [IFACE]              read only: what every radio offers for a peer-to-peer link (AirDrop's AWDL:
//!                                    docs/hardware.md 4a): each wiphy's interface types and combinations, action
//!                                    frames, off-channel sends, NAN, Wi-Fi Direct, vendor commands; every interface;
//!                                    Google's Wonder radio (its subcommands under 00:1a:11, who holds it); the verdicts
//!                                    for IFACE's radio (default wlan0); Wonder's module, driver and files,
//!                                    mosey_server; the network devices, modules and processes whose names hint at
//!                                    AWDL or NAN. Safe beside One UI and in a session.
//!   wifictl wonder [--seconds N] [--freq MHZ] [--country CC] [--station IFACE]
//!                                    a session on Wonder's raw 802.11 path beside wlan0 (wifi::wonder): a monitor
//!                                    interface on Wonder's radio, its country (the driver's own), frequency (2437,
//!                                    5220 or 5745 MHz; default 2437), TX rate and AWDL's BSSID set, up (the driver's
//!                                    PASSTHRU vdev wondertap0 opens), N s (default 30) of frames counted (AWDL's
//!                                    action and data frames, by peer), then all of it closed in reverse and wlan0's
//!                                    link checked against its link before. Refused when Wonder is missing or in use.
//!                                    Root, One UI running; Ctrl-C ends the count and still closes everything.
//! Each ends with "ok" or "failed". C: tools/wifictl.c.

use onebridges::hal::binder::strerror;
use onebridges::log::{set_sink, Level, Sink};
use onebridges::net::{NetBackend, NetConfig};
use onebridges::proto::wire::Wire;
use onebridges::wifi::chip;
use onebridges::wifi::p2p::WONDER_IFNAME;
use onebridges::wifi::proto::*;
use onebridges::wifi::supplicant::{security_parse, Supplicant, SUP_SERVICE};
use onebridges::wifi::wonder::{self, WonderParams, WonderResult};
use onebridges::wifi::{frames, station, wlan};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use xh_core::parse::atoi;

fn security_text(ies: &[u8], capability: u16) -> String {
    let s = security_parse(ies);
    if !s.rsn && !s.wpa {
        return (if capability & 0x10 != 0 { "WEP" } else { "open" }).to_owned();
    }
    let mut t = String::new();
    if s.sae {
        t.push_str("WPA3 ");
    }
    if s.psk || s.psk_sha256 {
        t.push_str(if s.rsn { "WPA2 " } else { "WPA " });
    }
    if s.eap {
        t.push_str("802.1X ");
    }
    if s.owe {
        t.push_str("OWE ");
    }
    if s.pmf_required {
        t.push_str("PMF required ");
    }
    if s.tkip && !s.ccmp {
        t.push_str("TKIP only");
    }
    t
}

fn print_bss(b: &OwBss, ies: &[u8]) {
    let name: String = b.ssid().iter().map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '?' }).collect();
    let m = b.bssid;
    println!(
        "  {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} {:4} MHz {:4} dBm  {:<22} \"{name}\"{}",
        m[0],
        m[1],
        m[2],
        m[3],
        m[4],
        m[5],
        b.freq_mhz,
        b.signal_dbm,
        security_text(ies, b.capability),
        if b.ssid_length != 0 { "" } else { " (hidden)" }
    );
}

fn scan(args: &[String]) -> i32 {
    let seconds = args.get(2).map_or(6, |s| atoi(s)) as u32;
    match wlan::scan_once(args.get(3).map_or("wlan0", String::as_str), seconds.wrapping_mul(1000), print_bss) {
        Err(rc) => {
            println!("scan: {}", strerror(rc));
            1
        }
        Ok(n) => {
            println!("scan: {n} networks");
            0
        }
    }
}

fn status(args: &[String]) -> i32 {
    let ifname = args.get(2).map_or("wlan0", String::as_str);
    let text = std::fs::read_to_string(format!("/sys/class/net/{ifname}/address")).unwrap_or_default();
    let text = text.lines().next().unwrap_or("");
    println!("{ifname}: {}", if text.is_empty() { "no such interface" } else { text });
    let Some(s) = Supplicant::open(ifname, 0, None) else { return 1 }; // a look: nothing registered, added or removed
    match s.status(32) {
        Ok(st) => {
            let m = st.mac;
            print!(
                "supplicant: service {SUP_SERVICE} (v{}), station interface {ifname}, address {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, {} network(s):",
                s.version(),
                m[0],
                m[1],
                m[2],
                m[3],
                m[4],
                m[5],
                st.networks.len()
            );
            for id in &st.networks {
                print!(" {id}");
            }
            let p = st.poll;
            print!("\nsignal poll: {}", if p[4] != 0 { "" } else { "none (not joined)\n" });
            if p[4] != 0 {
                println!("{} dBm, tx {} Mb/s, rx {} Mb/s, {} MHz", p[1], p[2], p[3], p[4]);
            }
            0
        }
        Err(_) => 1,
    }
}

// ---- join: this tool as the guest's driver ----
#[derive(Default)]
struct Host {
    link: u32,
    radio: u32,
    scan_done: bool,
    join: Option<OwJoinResult>,
    offer: Option<([u8; 4], [u8; 4])>, // the address offered, the server
    frames: u32,
}

#[derive(Default)]
struct Seen {
    host: Mutex<Host>,
    cond: Condvar,
}

impl Seen {
    /// Waits up to `ms` for `done`; whether it came.
    fn wait(&self, ms: u64, done: impl Fn(&Host) -> bool) -> bool {
        let h = self.host.lock().unwrap();
        let (h, _) = self.cond.wait_timeout_while(h, Duration::from_millis(ms), |h| !done(h)).unwrap();
        done(&h)
    }

    fn host_frame(&self, frame: &[u8]) {
        let mut h = self.host.lock().unwrap();
        if frames::ethertype(frame) != OW_ETHERTYPE {
            h.frames += 1;
            // A DHCP reply for us: IPv4, UDP 67 -> 68, BOOTREPLY with our transaction.
            if frame.len() >= 14 + 20 + 8 + 240 && frame[12..14] == [0x08, 0x00] && frame[23] == 17 {
                let udp = 14 + (frame[14] & 15) as usize * 4;
                if udp + 8 + 240 <= frame.len()
                    && frame[udp..udp + 4] == [0, 67, 0, 68]
                    && frame[udp + 8] == 2
                    && frame[udp + 12..udp + 16] == *b"\x4f\x57\x49\x46"
                    && h.offer.is_none()
                {
                    let ip: [u8; 4] = frame[udp + 8 + 16..udp + 8 + 20].try_into().unwrap();
                    let server: [u8; 4] = frame[26..30].try_into().unwrap();
                    h.offer = Some((ip, server));
                }
            }
            self.cond.notify_all();
            return;
        }
        let Some((m, p)) = message(frame) else { return };
        match m.kind {
            OW_STATE => {
                if let Some(s) = OwState::parse(p) {
                    if s.link != h.link || s.radio != h.radio {
                        println!(
                            "  state: radio {}, link {}{}, {} dBm, {} Mb/s, {} MHz, reason {}",
                            s.radio,
                            s.link,
                            if s.link == OW_LINK_UP { " (up)" } else { "" },
                            s.rssi_dbm,
                            s.tx_rate_mbps,
                            s.bss.freq_mhz,
                            s.reason
                        );
                    }
                    h.link = s.link;
                    h.radio = s.radio;
                }
            }
            OW_BSS => {
                if let Some(b) = OwBss::parse(p) {
                    let ies = &p[OwBss::SIZE..];
                    print_bss(&b, &ies[..ies.len().min(b.ie_length as usize)]);
                }
            }
            OW_SCAN_DONE => {
                if let Some(d) = OwScanDone::parse(p) {
                    println!("  scan done: {} networks (error {})", d.count, d.error);
                    h.scan_done = true;
                }
            }
            OW_JOIN_RESULT => h.join = OwJoinResult::parse(p),
            _ => {}
        }
        self.cond.notify_all();
    }
}

fn ip_sum(p: &[u8]) -> u16 {
    let mut sum: u32 = p.chunks(2).map(|w| u32::from(w[0]) << 8 | u32::from(*w.get(1).unwrap_or(&0))).sum();
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn dhcp_discover(net: &dyn NetBackend, mac: &[u8; 6]) {
    let mut f = vec![0u8; 14 + 20 + 8 + 300];
    f[..6].fill(0xff);
    f[6..12].copy_from_slice(mac);
    f[12] = 0x08;
    let ip_len = (f.len() - 14) as u16;
    {
        let ip = &mut f[14..34];
        ip[0] = 0x45;
        ip[2..4].copy_from_slice(&ip_len.to_be_bytes());
        ip[8] = 64;
        ip[9] = 17;
        ip[16..20].fill(0xff);
        let sum = ip_sum(ip);
        ip[10..12].copy_from_slice(&sum.to_be_bytes());
    }
    let udp = 34;
    f[udp + 1] = 68;
    f[udp + 3] = 67;
    f[udp + 4..udp + 6].copy_from_slice(&((8 + 300) as u16).to_be_bytes());
    let b = udp + 8;
    f[b..b + 3].copy_from_slice(&[1, 1, 6]);
    f[b + 4..b + 8].copy_from_slice(b"\x4f\x57\x49\x46"); // the transaction
    f[b + 10] = 0x80; // broadcast the reply
    f[b + 28..b + 34].copy_from_slice(mac);
    f[b + 236..b + 240].copy_from_slice(b"\x63\x82\x53\x63");
    f[b + 240..b + 249].copy_from_slice(b"\x35\x01\x01\x37\x03\x01\x03\x06\xff"); // DISCOVER; ask for the mask, the router, DNS
    if let Err(e) = net.send(&f) {
        println!("  DHCP discover not sent: {}", strerror(e));
    }
}

fn join(args: &[String]) -> i32 {
    let ssid = &args[2];
    let pass = args.get(3).map_or("", String::as_str);
    let stay = args.get(4).map_or(5, |s| atoi(s)) as u64;
    if ssid.len() > 32 || (!pass.is_empty() && !(8..=63).contains(&pass.len())) {
        println!("a name of at most 32 bytes, a passphrase of 8 to 63");
        return 1;
    }
    let Some((open, mac)) = wlan::prepare("wlan0", Some("/data/local/tmp")) else { return 1 }; // /data/local/tmp/wifi.state
    let seen = Arc::new(Seen::default());
    let s2 = seen.clone();
    let Some(net) = open(
        &NetConfig::default(),
        Arc::new(move |f: &[u8]| {
            s2.host_frame(f);
            true
        }),
    ) else {
        wlan::close();
        return 1;
    };
    let to_host = |kind: u16, payload: &[u8]| {
        let _ = net.send(&station::guest_frame(&mac, kind, payload));
    };
    let mut rc = 1;
    to_host(OW_HELLO, &OwHello { version: OW_VERSION }.to_bytes());
    'out: {
        if !seen.wait(10_000, |h| h.radio != 0) {
            println!("the bridge did not answer");
            break 'out;
        }
        std::thread::sleep(Duration::from_secs(2)); // the supplicant: found, or started by the lookup
        to_host(OW_SCAN, &[]);
        seen.wait(12_000, |h| h.scan_done);
        let security = if pass.is_empty() { OW_SECURITY_OPEN } else { OW_SECURITY_AUTO };
        let mut j = OwJoin::new(ssid.as_bytes(), security, pass.as_bytes(), false);
        let mut bytes = j.to_bytes();
        to_host(OW_JOIN, &bytes);
        j.wipe();
        bytes.fill(0);
        seen.wait(32_000, |h| h.join.is_some());
        let result = seen.host.lock().unwrap().join;
        match result {
            Some(r) if r.status == OW_JOIN_OK => {}
            _ => {
                let (status, code) = result.map_or((u32::MAX, 0), |r| (r.status, r.code));
                println!(
                    "join: {} (status {status}, 802.11 code {code})",
                    if result.is_none() { "no result in 32 s" } else { "failed" }
                );
                break 'out;
            }
        }
        println!("join: joined");
        for _ in 0..4 {
            dhcp_discover(&*net, &mac);
            if seen.wait(2000, |h| h.offer.is_some()) {
                break;
            }
        }
        let (offer, frames) = {
            let h = seen.host.lock().unwrap();
            (h.offer, h.frames)
        };
        match offer {
            Some((ip, server)) => println!(
                "DHCP: {}.{}.{}.{} offered by {}.{}.{}.{}",
                ip[0], ip[1], ip[2], ip[3], server[0], server[1], server[2], server[3]
            ),
            None => println!("DHCP: no offer in 8 s ({frames} other frames came from the network)"),
        }
        std::thread::sleep(Duration::from_secs(stay));
        rc = if offer.is_some() { 0 } else { 1 };
    }
    to_host(OW_LEAVE, &[]);
    std::thread::sleep(Duration::from_millis(500));
    wlan::close();
    rc
}

// ---- p2p: what the radio offers for AirDrop's AWDL (docs/hardware.md "AirDrop") ----
/// A name that hints at a peer-to-peer link of the vendor's or Google's: AWDL, NAN / Wi-Fi Aware, Pixel's "wonder"
/// interface and its mosey_server, Quick Share. [I] hints only: the names are guesses, the nl80211 report is the fact.
fn p2p_hint(name: &str) -> bool {
    const WORDS: [&str; 9] = ["awdl", "aware", "nan", "wonder", "mosey", "p2p", "quickshare", "nearby", "llw"];
    let lower: String = name.chars().take(255).map(|c| c.to_ascii_lowercase()).collect();
    WORDS.iter().any(|w| lower.contains(w))
}

fn exists(path: &str) -> bool {
    std::path::Path::new(path).exists() // C: access(F_OK)
}

fn p2p(args: &[String]) -> i32 {
    let ifname = args.get(2).map_or("wlan0", String::as_str);
    let radios = match wlan::radios() {
        Ok(r) => r, // every wiphy: Wonder's radio is not wlan0's
        Err(rc) => {
            println!("p2p: nl80211: {}", strerror(rc));
            return 1;
        }
    };
    print!("{}", radios.report(Some(ifname)));
    // Wonder's pieces on disk and in the kernel (Samsung's binding: a platform driver "wonder" on google,wonder-drv-v1;
    // Google's mosey_server in system_ext from One UI 9). Named paths only.
    const WONDER_PATHS: [&str; 7] = [
        "/sys/module/wonder",
        "/sys/module/wonder/parameters/physical_name",
        "/sys/bus/platform/drivers/wonder",
        "/vendor_dlkm/lib/modules/wonder.ko",
        "/vendor/lib/modules/wonder.ko",
        "/system_ext/bin/mosey_server",
        "/vendor/bin/mosey_server",
    ];
    for p in WONDER_PATHS {
        if !exists(p) {
            println!("wonder: {p}: none");
            continue;
        }
        let value = if p.contains("/parameters/") {
            std::fs::read_to_string(p)
                .ok()
                .and_then(|t| t.lines().next().map(|l| l.chars().take(63).collect()))
                .unwrap_or_default()
        } else {
            String::new()
        };
        println!("wonder: {p}: present{}{value}", if value.is_empty() { "" } else { ": " });
    }
    // Network devices: one with no wiphy but a hinting name is a virtual one (a tun of a user-space AWDL, Pixel's
    // "wonder" virtual radio).
    if let Ok(d) = std::fs::read_dir("/sys/class/net") {
        for e in d.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let radio = exists(&format!("/sys/class/net/{name}/phy80211"));
            if radio || p2p_hint(&name) {
                println!("netdev: {name}{}", if radio { " (a Wi-Fi radio's)" } else { " (no Wi-Fi radio: virtual)" });
            }
        }
    }
    if let Ok(modules) = std::fs::read_to_string("/proc/modules") {
        for line in modules.lines() {
            let name = line.split(' ').next().unwrap_or("");
            if p2p_hint(name) || name.contains("qca") || name.contains("cnss") {
                println!("module: {name}");
            }
        }
    }
    if let Ok(d) = std::fs::read_dir("/proc") {
        for e in d.flatten() {
            let pid = e.file_name().to_string_lossy().into_owned();
            if !pid.starts_with(|c: char| ('1'..='9').contains(&c)) {
                continue;
            }
            let Ok(cmd) = std::fs::read(format!("/proc/{pid}/cmdline")) else { continue };
            // the program's path, up to its first NUL
            let cmd = &cmd[..cmd.len().min(255)];
            let cmd = xh_core::text::nul_text(cmd);
            if !cmd.is_empty() && p2p_hint(&cmd) {
                println!("process: {pid} {cmd}");
            }
        }
    }
    0
}

// ---- wonder: a session on Wonder's raw 802.11 path (docs/hardware.md 4a, wifi::wonder) ----
static WONDER_STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn wonder_signal(_sig: libc::c_int) {
    WONDER_STOP.store(true, Ordering::Relaxed);
}

fn wonder(args: &[String]) -> i32 {
    let mut p = WonderParams {
        station: "wlan0",
        name: WONDER_IFNAME,
        freq_mhz: 2437,
        seconds: 30,
        stop: Some(&WONDER_STOP),
        ..Default::default()
    };
    let mut i = 2;
    while i < args.len() {
        let v = args.get(i + 1);
        match (args[i].as_str(), v) {
            ("--seconds", Some(v)) => p.seconds = atoi(v) as u32,
            ("--freq", Some(v)) => p.freq_mhz = atoi(v) as u32,
            ("--country", Some(v)) if v.len() == 2 => {
                p.country = v.chars().map(|c| ((c as u8) & !0x20) as char).collect()
            }
            ("--station", Some(v)) => p.station = v,
            _ => {
                println!(
                    "usage: wifictl wonder [--seconds 1..600] [--freq 2437|5220|5745] [--country CC] [--station IFACE]"
                );
                return 2;
            }
        }
        i += 2;
    }
    if p.seconds == 0 || p.seconds > 600 {
        println!("wonder: --seconds 1 to 600");
        return 2;
    }
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        println!("wonder: needs root (su)");
        return 1;
    }
    // SAFETY: a handler that only stores an atomic; no SA_RESTART: the receive's poll ends at once. SIGPIPE ignored:
    // adb gone, the lines are lost, the teardown still runs.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = wonder_signal as extern "C" fn(libc::c_int) as usize;
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let Some(mut ops) = wlan::PhoneWonder::open() else {
        println!("wonder: nl80211 not opened");
        return 1;
    };
    let mut r = WonderResult::default();
    let rc = wonder::run(&mut ops, &p, &mut r);
    drop(ops);
    println!(
        "wonder: {}: {} frames, {} AWDL action frames from {} peer(s); {} {}",
        rc.map_or_else(strerror, |()| "done".to_owned()),
        r.count.frames,
        r.count.awdl_action,
        r.count.peers.len(),
        p.station,
        if !r.link_before {
            "was not associated before"
        } else if r.link_after {
            "associated as before"
        } else {
            "LOST ITS LINK"
        }
    );
    i32::from(rc.is_err())
}

fn chip_cmd(args: &[String]) -> i32 {
    match args.get(2).map_or("status", String::as_str) {
        "status" => match chip::status(2000) {
            Some(st) => {
                println!("wi-fi: {}", st.text());
                0
            }
            None => 1,
        },
        "start" => {
            // The session's own path: the HAL started, then the wait for the driver's station interface.
            if !chip::ensure("wlan0", chip::START_MS, || wlan::present("wlan0")) {
                return 1;
            }
            println!("wlan0: {}", if wlan::present("wlan0") { "there" } else { "not there" });
            0
        }
        "stop" => match chip::stop(2000) {
            Ok(()) => {
                println!("the chip is stopped (One UI's framework starts it again when its Wi-Fi is on)");
                0
            }
            Err(e) => {
                println!("  {e}");
                1
            }
        },
        _ => 2,
    }
}

fn main() {
    set_sink(Sink::Custom(Arc::new(|level: Level, line: &str| {
        println!("  {}{line}", if level == Level::Error { "error: " } else { "" })
    })));
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "usage: wifictl status [IFACE] | scan [SECONDS [IFACE]] | join SSID [PASSPHRASE [SECONDS]] | restore FILE | chip [start|stop|status] | p2p [IFACE] | wonder [--seconds N] [--freq MHZ] [--country CC] [--station IFACE]"
        );
        std::process::exit(2);
    }
    let rc = match args[1].as_str() {
        "status" => status(&args),
        "scan" => scan(&args),
        "chip" => chip_cmd(&args),
        "p2p" => p2p(&args),
        "wonder" => wonder(&args),
        "join" if args.len() > 2 => join(&args),
        "restore" if args.len() > 2 => match wlan::restore(&args[2]) {
            Err(rc) if rc == -libc::ENOENT => {
                println!("nothing to give back (no {})", args[2]);
                0
            }
            Err(_) => 1,
            Ok(()) => 0,
        },
        _ => 2,
    };
    println!(
        "{}",
        if rc == 0 {
            "ok"
        } else if rc == 2 {
            "unknown command"
        } else {
            "failed"
        }
    );
    std::process::exit(rc);
}
