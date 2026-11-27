// SPDX-License-Identifier: MIT
//! The supplicant client (wifi::supplicant on hal::aidl) against a real binder driver: a private binderfs on the build
//! host (the test runs itself in a user namespace with one: hal::binderfs). A child process plays the service manager
//! (the context manager) and android.hardware.wifi.supplicant.ISupplicant/default with one station interface and its
//! networks, and calls back as wpa_supplicant's AIDL service does: the descriptor and the interface version of the
//! callback asked for inside registerCallback (a call back into the client before its call is answered), the state
//! changes one-way. The service reports interface version 3, as the base S25's image does; the client's version 2
//! codes are version 3's (version 3 appended its methods and callbacks after version 2's).
//!
//! Scenarios: a service that is not registered at first (a lazy service: the lookup is tried again); no station
//! interface (added, and removed at the close); One UI's networks removed; a WPA2 join that completes, with what the
//! network got; a wrong key (the access point ends the 4-way handshake); a network that is not there; an association
//! rejected; a link that drops; a look without a callback (the tool beside One UI); a supplicant that dies.
//! Both ends use hal::aidl: what this proves is the kernel protocol (objects, handles, nested calls, one-way order,
//! death). The parcels' bytes are checked in wifi's unit tests; the real libbinder and the vendor's supplicant are
//! `wifictl status` on the phone. C: tests/test_aidl_binder.c.

mod common;

use onebridges::hal::aidl::{self, Connection, Object, Parcel, Reader};
use onebridges::hal::binderfs::{self, Entered};
use onebridges::wifi::chip::{
    self, CHIP_CREATE_STA_IFACE, CHIP_GET_STA_IFACE_NAMES, WIFI_GET_CHIP, WIFI_ICHIP, WIFI_IS_STARTED, WIFI_IWIFI,
    WIFI_SERVICE, WIFI_START, WIFI_STA_IFACE, WIFI_STOP,
};
use onebridges::wifi::proto::*;
use onebridges::wifi::supplicant::*;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

// ================================ the fake service manager and supplicant (child) ================================
const OBJ_MANAGER: u64 = 0;
const OBJ_SUPPLICANT: u64 = 1;
const OBJ_IFACE: u64 = 2;
const OBJ_WIFI: u64 = 3;
const OBJ_WIFI_CHIP: u64 = 4;
const OBJ_WIFI_STA: u64 = 5;
const OBJ_NETWORK: u64 = 0x100;

struct Fake {
    log: i32, // what the client did, a line each, for the parent
    lookups: u32,
    have_iface: bool,
    have_callback: bool,
    callback: u32,
    next_network: i32, // 0 and 1 are "One UI's"
    present: [bool; 16],
    ssid: String,
    wifi_lookups: u32,
    wifi_version: i32, // what the Wi-Fi HAL's getInterfaceVersion answers
    wifi_started: bool,
    wifi_iface: bool, // the chip has a station interface (getStaIfaceNames; createStaIface makes one)
}

static FAKE: OnceLock<Connection> = OnceLock::new();
static STATE: Mutex<Option<Fake>> = Mutex::new(None);
static SFAIL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

macro_rules! scheck {
    ($cond:expr) => {{
        if !($cond) {
            SFAIL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            println!("FAKE FAIL {}:{}: {}", file!(), line!(), stringify!($cond));
        }
    }};
}

fn with<T>(f: impl FnOnce(&mut Fake) -> T) -> T {
    f(STATE.lock().unwrap().as_mut().expect("the fake's state"))
}

fn note(line: &str) {
    let fd = with(|s| s.log);
    let text = format!("{line}\n");
    // SAFETY: our pipe's write end and our text.
    unsafe { libc::write(fd, text.as_ptr().cast(), text.len()) };
}

fn conn() -> &'static Connection {
    FAKE.get().expect("the fake's binder")
}

fn oneway(code: u32, p: &Parcel) {
    let cb = with(|s| s.callback);
    scheck!(conn().oneway(cb, code, p).is_ok());
}

fn state(new_state: i32, modern: bool, frequency: i32) {
    let bssid = [1u8, 2, 3, 4, 5, 6];
    let (id, ssid) = with(|s| (s.next_network - 1, s.ssid.clone()));
    let mut p = Parcel::with_token(SUP_STA_CALLBACK);
    if modern {
        // onSupplicantStateChanged(SupplicantStateChangeData)
        p.put_parcelable(|p| {
            p.put_i32(new_state);
            p.put_i32(id);
            p.put_bytes(ssid.as_bytes());
            p.put_bytes(&bssid);
            p.put_i32(KM_WPA_PSK as i32);
            p.put_i32(frequency);
            p.put_i32(0);
        });
        oneway(CB_SUPPLICANT_STATE_CHANGED, &p);
    } else {
        // onStateChanged(newState, bssid, id, ssid, filsHlpSent)
        p.put_i32(new_state);
        p.put_bytes(&bssid);
        p.put_i32(id);
        p.put_bytes(ssid.as_bytes());
        p.put_i32(0);
        oneway(CB_STATE_CHANGED, &p);
    }
}

fn disconnected(local: bool, reason: i32) {
    let mut p = Parcel::with_token(SUP_STA_CALLBACK);
    p.put_bytes(&[1, 2, 3, 4, 5, 6]);
    p.put_i32(i32::from(local));
    p.put_i32(reason);
    oneway(CB_DISCONNECTED, &p);
}

/// What the network named ssid does once selected.
fn selected() {
    let (have_callback, ssid) = with(|s| (s.have_callback, s.ssid.clone()));
    if !have_callback {
        return;
    }
    disconnected(true, 3); // as the real one: the old association ends by the phone's own doing
    state(ST_SCANNING, false, 0);
    if ssid == "missing" {
        for _ in 0..2 {
            let mut p = Parcel::with_token(SUP_STA_CALLBACK);
            p.put_bytes(ssid.as_bytes());
            oneway(CB_NETWORK_NOT_FOUND, &p);
        }
        return;
    }
    state(ST_ASSOCIATING, false, 0);
    if ssid == "reject" {
        // onAssociationRejected(AssociationRejectionData)
        let mut p = Parcel::with_token(SUP_STA_CALLBACK);
        p.put_parcelable(|p| {
            p.put_bytes(ssid.as_bytes());
            p.put_bytes(b"\x01\x02\x03\x04\x05\x06");
            for v in [17, 0, 0, 0, 0] {
                p.put_i32(v);
            }
        });
        oneway(CB_ASSOCIATION_REJECTED, &p);
        return;
    }
    state(ST_ASSOCIATED, false, 0);
    state(ST_FOURWAY_HANDSHAKE, false, 0);
    if ssid == "wrongkey" {
        disconnected(false, 15);
        state(ST_DISCONNECTED, false, 0);
        return;
    }
    state(ST_GROUP_HANDSHAKE, true, 5180);
    state(ST_COMPLETED, true, 5180);
    if ssid == "drops" {
        std::thread::sleep(Duration::from_millis(200));
        disconnected(false, 4);
        state(ST_DISCONNECTED, true, 0);
    }
}

fn fake_incoming(object: u64, code: u32, r: &mut Reader<'_>) -> Result<Parcel, i32> {
    let mut reply = Parcel::new();
    if object == OBJ_MANAGER {
        let name = if code == SM_GET_SERVICE && r.token(SUP_MANAGER) { r.string(95) } else { None };
        let name = name.ok_or(aidl::UNKNOWN_TRANSACTION)?;
        reply.put_ok();
        // The Wi-Fi HAL: a lazy service too (null on the first lookup, as init starts it).
        if name == WIFI_SERVICE {
            let lookups = with(|s| {
                s.wifi_lookups += 1;
                s.wifi_lookups
            });
            if lookups <= 1 {
                reply.put_null_binder();
            } else {
                reply.put_local(OBJ_WIFI);
            }
            return Ok(reply);
        }
        let lookups = with(|s| {
            s.lookups += 1;
            s.lookups
        });
        if name != SUP_SERVICE || lookups <= 2 {
            reply.put_null_binder(); // a lazy service: there on the third lookup
        } else {
            reply.put_local(OBJ_SUPPLICANT);
        }
        return Ok(reply);
    }
    if object == OBJ_WIFI {
        if !r.token(WIFI_IWIFI) {
            return Err(aidl::BAD_TYPE);
        }
        match code {
            aidl::GET_VERSION => {
                reply.put_ok();
                reply.put_i32(with(|s| s.wifi_version));
            }
            WIFI_IS_STARTED => {
                reply.put_ok();
                reply.put_i32(i32::from(with(|s| s.wifi_started)));
            }
            WIFI_START => {
                scheck!(!with(|s| s.wifi_started));
                with(|s| s.wifi_started = true);
                note("start");
                reply.put_ok();
            }
            WIFI_STOP => {
                scheck!(with(|s| s.wifi_started));
                with(|s| s.wifi_started = false);
                note("stop");
                reply.put_ok();
            }
            WIFI_GET_CHIP => {
                let id = r.i32();
                scheck!(id == 0);
                note("getChip 0");
                reply.put_ok();
                reply.put_local(OBJ_WIFI_CHIP);
            }
            _ => return Err(aidl::UNKNOWN_TRANSACTION),
        }
        return Ok(reply);
    }
    if object == OBJ_WIFI_CHIP {
        if !r.token(WIFI_ICHIP) {
            return Err(aidl::BAD_TYPE);
        }
        match code {
            CHIP_GET_STA_IFACE_NAMES => {
                let iface = with(|s| s.wifi_iface);
                reply.put_ok();
                reply.put_i32(i32::from(iface));
                if iface {
                    reply.put_str("wlan0");
                }
            }
            CHIP_CREATE_STA_IFACE => {
                scheck!(!with(|s| s.wifi_iface));
                with(|s| s.wifi_iface = true);
                note("createStaIface");
                reply.put_ok();
                reply.put_local(OBJ_WIFI_STA);
            }
            _ => return Err(aidl::UNKNOWN_TRANSACTION),
        }
        return Ok(reply);
    }
    if object == OBJ_WIFI_STA {
        if !r.token(WIFI_STA_IFACE) {
            return Err(aidl::BAD_TYPE);
        }
        // Every method of the station interface: the client here holds none (it created one only to let go of it).
        return Err(aidl::UNKNOWN_TRANSACTION);
    }
    if object == OBJ_SUPPLICANT {
        if !r.token(SUP_ISUPPLICANT) {
            return Err(aidl::BAD_TYPE);
        }
        match code {
            aidl::GET_VERSION => {
                // The base S25's image serves version 3; the calls the client makes are version 2's and v3 kept them.
                reply.put_ok();
                reply.put_i32(3);
                return Ok(reply);
            }
            SUP_GET_STA_INTERFACE | SUP_ADD_STA_INTERFACE => {
                let text = r.string(95);
                scheck!(text.as_deref() == Some("wlan0"));
                if code == SUP_GET_STA_INTERFACE && !with(|s| s.have_iface) {
                    reply.put_service_error(4); // FAILURE_IFACE_UNKNOWN
                    return Ok(reply);
                }
                if code == SUP_ADD_STA_INTERFACE {
                    scheck!(!with(|s| s.have_iface));
                    with(|s| s.have_iface = true);
                    note(&format!("addStaInterface {}", text.unwrap_or_default()));
                }
                reply.put_ok();
                reply.put_local(OBJ_IFACE);
                return Ok(reply);
            }
            SUP_REMOVE_INTERFACE => {
                // IfaceInfo: type, name
                let end = r.parcelable();
                let kind = r.i32();
                let text = r.string(95);
                scheck!(end.is_some() && kind == 0 && text.as_deref() == Some("wlan0") && Some(r.position()) == end);
                with(|s| s.have_iface = false);
                note(&format!("removeInterface {}", text.unwrap_or_default()));
                reply.put_ok();
                return Ok(reply);
            }
            _ => return Err(aidl::UNKNOWN_TRANSACTION),
        }
    }
    if object == OBJ_IFACE {
        if !r.token(SUP_STA_IFACE) {
            return Err(aidl::BAD_TYPE);
        }
        match code {
            STA_REGISTER_CALLBACK => {
                let cb = match r.binder() {
                    Object::Handle(h) => Some(h),
                    _ => None,
                };
                scheck!(cb.is_some());
                let cb = cb.unwrap_or(0);
                scheck!(conn().acquire(cb).is_ok() && conn().watch(cb, 1).is_ok());
                with(|s| s.callback = cb);
                // As libbinder_ndk's fromBinder and the supplicant do: the descriptor and the version, before the answer.
                let descriptor =
                    conn().call(cb, aidl::INTERFACE, &Parcel::new(), |r| r.string(95).ok_or(-libc::EPROTO));
                scheck!(descriptor.as_deref() == Ok(SUP_STA_CALLBACK));
                let version = conn().call(cb, aidl::GET_VERSION, &Parcel::with_token(SUP_STA_CALLBACK), aidl::take_i32);
                scheck!(version == Ok(SUP_VERSION));
                scheck!(conn().call_void(cb, aidl::PING, &Parcel::new()).is_ok());
                let dump = conn().call_void(cb, 0x5f44_4d50 /* dump */, &Parcel::with_token(SUP_STA_CALLBACK));
                scheck!(dump.map_err(|e| e.rc) == Err(-libc::ENOSYS));
                with(|s| s.have_callback = true);
                note("registerCallback");
                reply.put_ok();
            }
            STA_LIST_NETWORKS => {
                let present = with(|s| s.present);
                reply.put_ok();
                reply.put_i32(present.iter().filter(|p| **p).count() as i32);
                for (i, _) in present.iter().enumerate().filter(|(_, p)| **p) {
                    reply.put_i32(i as i32);
                }
            }
            STA_REMOVE_NETWORK => {
                let id = r.i32();
                let ok = with(|s| {
                    let ok = (0..16).contains(&id) && s.present[id as usize];
                    if ok {
                        s.present[id as usize] = false;
                    }
                    ok
                });
                if !ok {
                    reply.put_service_error(9);
                    return Ok(reply);
                }
                note(&format!("removeNetwork {id}"));
                reply.put_ok();
            }
            STA_ADD_NETWORK => {
                let n = with(|s| {
                    let n = s.next_network;
                    if n < 16 {
                        s.present[n as usize] = true;
                    }
                    s.next_network += 1;
                    n
                });
                scheck!(n < 16);
                reply.put_ok();
                reply.put_local(OBJ_NETWORK + n as u64);
                note(&format!("addNetwork {n}"));
            }
            STA_DISCONNECT => {
                note("disconnect");
                reply.put_ok();
            }
            STA_GET_MAC_ADDRESS => {
                reply.put_ok();
                reply.put_bytes(b"\x02\x11\x22\x33\x44\x55");
            }
            STA_GET_SIGNAL_POLL_RESULTS => {
                reply.put_ok();
                for v in [1, 1, 24, 0, -51, 866, 780, 5180] {
                    reply.put_i32(v);
                }
            }
            _ => return Err(aidl::UNKNOWN_TRANSACTION),
        }
        return Ok(reply);
    }
    if (OBJ_NETWORK..OBJ_NETWORK + 16).contains(&object) {
        if !r.token(SUP_STA_NETWORK) {
            return Err(aidl::BAD_TYPE);
        }
        reply.put_ok();
        let i32_note = |name: &str, r: &mut Reader<'_>| note(&format!("{name} {}", r.i32()));
        match code {
            NET_GET_ID => reply.put_i32((object - OBJ_NETWORK) as i32),
            NET_SET_SSID => {
                let b = r.bytes();
                scheck!(b.is_some_and(|b| b.len() <= 32));
                let ssid = String::from_utf8_lossy(b.unwrap_or_default()).into_owned();
                note(&format!("setSsid {ssid}"));
                with(|s| s.ssid = ssid);
            }
            NET_SET_KEY_MGMT => i32_note("setKeyMgmt", r),
            NET_SET_PROTO => i32_note("setProto", r),
            NET_SET_PAIRWISE_CIPHER => i32_note("setPairwiseCipher", r),
            NET_SET_GROUP_CIPHER => i32_note("setGroupCipher", r),
            NET_SET_REQUIRE_PMF => i32_note("setRequirePmf", r),
            NET_SET_SCAN_SSID => i32_note("setScanSsid", r),
            NET_SET_PSK_PASSPHRASE | NET_SET_SAE_PASSWORD => {
                let text = r.string(95);
                scheck!(text.is_some());
                let name = if code == NET_SET_PSK_PASSPHRASE { "setPskPassphrase" } else { "setSaePassword" };
                note(&format!("{name} {}", text.unwrap_or_default()));
            }
            NET_SET_SAE_H2E_MODE => return Err(aidl::UNKNOWN_TRANSACTION), // an older supplicant: no such call
            NET_SET_PSK => {
                let b = r.bytes().unwrap_or_default();
                scheck!(b.len() == 32);
                if b.len() == 32 {
                    note(&format!("setPsk {:02x}..{:02x}", b[0], b[31]));
                }
            }
            NET_SELECT => {
                note(&format!("select {}", object - OBJ_NETWORK));
                selected();
            }
            _ => return Err(aidl::UNKNOWN_TRANSACTION),
        }
        return Ok(reply);
    }
    Err(aidl::UNKNOWN_TRANSACTION)
}

fn run_fake(log: i32, with_iface: bool, wifi: (i32, bool, bool)) {
    let mut present = [false; 16];
    present[0] = true;
    present[1] = true;
    *STATE.lock().unwrap() = Some(Fake {
        log,
        lookups: 0,
        have_iface: with_iface,
        have_callback: false,
        callback: 0,
        next_network: 2,
        present,
        ssid: String::new(),
        wifi_lookups: 0,
        wifi_version: wifi.0,
        wifi_started: wifi.1,
        wifi_iface: wifi.2,
    });
    let c = Connection::open(
        None,
        Some(Box::new(|object, code, _one_way, r: &mut Reader<'_>| fake_incoming(object, code, r))),
        Some(Box::new(|_cookie| {
            note("the client died");
            with(|s| s.have_callback = false);
        })),
    );
    let Ok(c) = c else {
        println!("fake: no binder");
        common::exit(91);
    };
    if c.context_manager().is_err() {
        println!("fake: not the context manager");
        common::exit(91);
    }
    let _ = FAKE.set(c);
    note("ready");
    loop {
        std::thread::park();
    }
}

// ================================ the client (parent) ================================
#[derive(Default)]
struct Events {
    got: Mutex<Vec<Event>>,
    cond: Condvar,
}

impl Events {
    fn func(self: &Arc<Self>) -> EventFn {
        let me = self.clone();
        Box::new(move |e| {
            let mut g = me.got.lock().unwrap();
            if g.len() < 8 {
                g.push(e);
            }
            me.cond.notify_all();
        })
    }
    /// At least n events within 3 s: a copy of them.
    fn wait(&self, n: usize) -> Vec<Event> {
        let g = self.got.lock().unwrap();
        let (g, _) = self.cond.wait_timeout_while(g, Duration::from_secs(3), |g| g.len() < n).unwrap();
        g.clone()
    }
    fn clear(&self) {
        self.got.lock().unwrap().clear();
    }
}

fn start_fake(with_iface: bool) -> (libc::pid_t, i32) {
    start_fake_wifi(with_iface, (1, true, true))
}

/// A fake whose Wi-Fi HAL is (version, started, has a station interface).
fn start_fake_wifi(with_iface: bool, wifi: (i32, bool, bool)) -> (libc::pid_t, i32) {
    let mut fds = [0; 2];
    // SAFETY: pipe writes two fds.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let pid = common::fork(move || {
        // SAFETY: the read end is the parent's.
        unsafe { libc::close(fds[0]) };
        run_fake(fds[1], with_iface, wifi);
    });
    // SAFETY: the write end is the child's.
    unsafe { libc::close(fds[1]) };
    let mut line = [0u8; 6];
    // SAFETY: our pipe and buffer.
    let got = unsafe { libc::read(fds[0], line.as_mut_ptr().cast(), 6) };
    check!(got == 6 && &line == b"ready\n");
    (pid, fds[0])
}

/// Everything the fake noted so far (it is stopped first, so the pipe ends).
fn stop_fake(pid: libc::pid_t, log: i32) -> String {
    common::kill(pid);
    let mut text = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        // SAFETY: our pipe and buffer.
        let n = unsafe { libc::read(log, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            break;
        }
        text.extend_from_slice(&buf[..n as usize]);
    }
    // SAFETY: our pipe's read end.
    unsafe { libc::close(log) };
    String::from_utf8_lossy(&text).into_owned()
}

fn main() {
    let device = match binderfs::enter("ONEIOS_BINDER", "binder") {
        Entered::Device(path) => path,
        Entered::Skipped(why) => {
            println!("test_aidl_binder: skipped ({why})");
            return;
        }
        Entered::Ran(code) => std::process::exit(code),
    };
    let _ = device;
    common::log_to_stdout();
    let ev = Arc::new(Events::default());

    // 1. A lazy service with no station interface; joins of every outcome.
    let (pid, log) = start_fake(false);
    check!(Supplicant::open("wlan0", 0, Some(ev.func())).is_none()); // not registered yet, and no time to wait
    let s = Supplicant::open("wlan0", 3000, Some(ev.func())); // the third lookup finds it
    check!(s.is_some());
    let Some(mut s) = s else {
        stop_fake(pid, log);
        std::process::exit(common::finish("aidl binder"));
    };
    check!(s.version() == 3); // the base S25's service; the client still uses the v2 codes
    let mut ies = [0u8; 64];
    ies[..28].copy_from_slice(&[
        0, 4, b'h', b'o', b'm', b'e', 48, 20, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f,
        0xac, 2, 0x0c, 0,
    ]);
    let j = OwJoin::new(b"home", OW_SECURITY_AUTO, b"passphrase", false);
    check!(s.join(&j, Some(&ies[..28])).is_ok());
    check!(ev.wait(1).first() == Some(&Event::Joined) && s.frequency() == 5180);
    let st = s.status(8);
    check!(st.as_ref().is_ok_and(|st| st.networks == [2]
        && st.mac[5] == 0x55
        && st.poll[1] == -51
        && st.poll[2] == 866
        && st.poll[4] == 5180));
    // A wrong key: the access point ends the handshake.
    ev.clear();
    check!(s.join(&OwJoin::new(b"wrongkey", OW_SECURITY_AUTO, b"12345678", false), Some(&ies[..28])).is_ok());
    check!(ev.wait(1).first() == Some(&Event::JoinFailed { status: OW_JOIN_AUTH, code: 15 }));
    // A network that is not there (two scans without it); no record of it: a probe for its name.
    ev.clear();
    check!(s.join(&OwJoin::new(b"missing", OW_SECURITY_OPEN, b"", false), None).is_ok());
    check!(matches!(ev.wait(1).first(), Some(Event::JoinFailed { status: OW_JOIN_NO_NETWORK, .. })));
    // An association rejected: the access point's status.
    ev.clear();
    let mut key = [0u8; 32];
    key[0] = 0xaa;
    key[31] = 0xbb;
    let reject = OwJoin::new(b"reject", OW_SECURITY_AUTO, &key, true);
    check!(s.join(&reject, Some(&ies[..28])).is_ok());
    check!(ev.wait(1).first() == Some(&Event::JoinFailed { status: OW_JOIN_REJECTED, code: 17 }));
    // A link that drops after the join.
    ev.clear();
    check!(s.join(&OwJoin::new(b"drops", OW_SECURITY_AUTO, b"12345678", false), Some(&ies[..28])).is_ok());
    let e = ev.wait(2);
    check!(e.len() >= 2 && e[0] == Event::Joined && e[1] == Event::Disconnected { code: 4 });
    check!(s.leave().is_ok());
    // A WPA3-only network with a WPA2 key: refused before the supplicant is asked.
    let mut sae = ies;
    sae[25] = 8;
    check!(s.join(&reject, Some(&sae[..28])) == Err(-libc::EINVAL));
    drop(s);
    std::thread::sleep(Duration::from_millis(100));
    let text = stop_fake(pid, log);
    // What the supplicant saw, in order: the interface added, the callback, One UI's two networks removed, the first
    // join's network (WPA2: 0x102 = WPA_PSK | WPA_PSK_SHA256, RSN, CCMP and the GCMPs) ...
    let expected = [
        "addStaInterface wlan0\nregisterCallback\ndisconnect\nremoveNetwork 0\nremoveNetwork 1\n",
        "disconnect\naddNetwork 2\nsetSsid home\nsetKeyMgmt 258\nsetProto 2\nsetPairwiseCipher 336\nsetGroupCipher 336\nsetPskPassphrase passphrase\nselect 2\n",
        "disconnect\nremoveNetwork 2\naddNetwork 3\nsetSsid wrongkey\n",
        // the open network no scan saw: no ciphers, a probe for the name
        "addNetwork 4\nsetSsid missing\nsetKeyMgmt 4\nsetScanSsid 1\nselect 4\n",
        "setSsid reject\nsetKeyMgmt 258\nsetProto 2\nsetPairwiseCipher 336\nsetGroupCipher 336\nsetPsk aa..bb\nselect 5\n",
        "select 6\ndisconnect\nremoveNetwork 6\n",
        // the close: nothing joined, the interface it added removed
        "removeInterface wlan0\n",
    ];
    let mut at = 0;
    for e in expected {
        let found = text[at..].find(e);
        check!(found.is_some());
        match found {
            Some(i) => at += i + e.len(),
            None => {
                println!("  not found after the last match: {e}  the fake's log:\n{text}");
                break;
            }
        }
    }
    check!(!text.contains("FAKE FAIL"));

    // 2. A supplicant that has the interface (One UI running): a look registers, adds and removes nothing.
    let (pid, log) = start_fake(true);
    let look = Supplicant::open("wlan0", 3000, None);
    check!(look.is_some());
    check!(look.as_ref().is_some_and(|look| look.version() == 3));
    if let Some(look) = look {
        let st = look.status(8);
        check!(st.is_ok_and(|st| st.networks == [0, 1]));
    }
    // The session's client on the same supplicant: the interface is there, so it is not removed at the close.
    let s = Supplicant::open("wlan0", 3000, Some(ev.func()));
    check!(s.is_some());
    drop(s);
    std::thread::sleep(Duration::from_millis(100));
    let text = stop_fake(pid, log);
    let want = "registerCallback\ndisconnect\nremoveNetwork 0\nremoveNetwork 1\ndisconnect\nthe client died\n";
    check!(text == want);
    if text != want {
        println!("  the fake's log:\n{text}");
    }

    // 3. A supplicant that dies while joined: the event, and calls fail without waiting.
    let (pid, log) = start_fake(true);
    let s = Supplicant::open("wlan0", 3000, Some(ev.func()));
    check!(s.is_some());
    ev.clear();
    if let Some(mut s) = s {
        check!(s.join(&j, Some(&ies[..28])).is_ok() && ev.wait(1).first() == Some(&Event::Joined));
        let _ = stop_fake(pid, log);
        let e = ev.wait(2);
        check!(e.len() >= 2 && e[1] == Event::Died);
        check!(s.leave() == Err(-libc::ENODEV));
        check!(s.join(&j, Some(&ies[..28])).is_err()); // no service manager any more
    }

    // 4. The Wi-Fi chip's HAL (docs/hardware.md section 3, "Wi-Fi off in One UI at the switch"): a lazy service, a
    // stopped chip, and no station interface. The start comes first; the first wait sees no wlan0, the chip has no
    // interface to offer, one is created, and the third look finds it.
    {
        let (pid, log) = start_fake_wifi(true, (1, false, false));
        let looks = std::sync::atomic::AtomicUsize::new(0);
        let started = chip::ensure("wlan0", 300, || looks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 4);
        check!(started);
        let st = chip::status(0);
        check!(st
            .as_ref()
            .is_some_and(|st| st.version == 1 && st.started && st.interfaces == vec!["wlan0".to_owned()]));
        if let Some(st) = &st {
            check!(st.text() == "IWifi v1: started, station interface(s): wlan0");
        }
        chip::give_back();
        std::thread::sleep(Duration::from_millis(100));
        let text = stop_fake(pid, log);
        // The start, the chip's names (none), the chip again for the interface created, the status's own look, and
        // the return's stop.
        let want = "start\ngetChip 0\ngetChip 0\ncreateStaIface\ngetChip 0\nstop\n";
        check!(text == want);
        if text != want {
            println!("  the fake's log:\n{text}");
        }
    }

    // 5. One UI's own Wi-Fi was on: the chip is already started, wlan0 is there, so nothing is started or stopped (a
    // newer interface version is accepted: its v1 methods come first).
    {
        let (pid, log) = start_fake_wifi(true, (2, true, true));
        check!(chip::ensure("wlan0", 300, || true));
        chip::give_back();
        std::thread::sleep(Duration::from_millis(100));
        let text = stop_fake(pid, log);
        check!(text.is_empty());
        if !text.is_empty() {
            println!("  the fake's log:\n{text}");
        }
    }

    // 6. Started, wlan0 not there, and the chip has a station interface of another name: no second one is created, and
    // the session says so instead of taking the wrong interface.
    {
        let (pid, log) = start_fake_wifi(true, (1, true, true));
        check!(!chip::ensure("wlan1", 300, || false));
        chip::give_back();
        std::thread::sleep(Duration::from_millis(100));
        let text = stop_fake(pid, log);
        check!(text == "getChip 0\n");
        if text != "getChip 0\n" {
            println!("  the fake's log:\n{text}");
        }
    }

    std::process::exit(common::finish("aidl binder"));
}
