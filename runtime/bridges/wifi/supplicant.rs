// SPDX-License-Identifier: MIT
//! The vendor's wpa_supplicant as the Wi-Fi bridge's station (docs/hardware.md section 3): its stable AIDL service
//! android.hardware.wifi.supplicant.ISupplicant/default, over /dev/binder with [`crate::hal::aidl`]. The S24 Ultra's
//! image serves the frozen interface version 2 (the hash 5e5c6d9e5565999f73fa7f797f5b94616174745b); the base S25's
//! serves version 3. Version 3 appended its methods and callbacks after version 2's and moved none of them, so the
//! codes used here are the same on both. The bridge is its only client in a session, as One UI's framework is otherwise.
//!
//! What it does on the phone, as One UI's framework does: finds the service, takes the station interface of wlan0
//! (adds it when the supplicant came up with none: it is a lazy service that ends when its client dies and starts on
//! the next lookup), registers one callback object, and for a join adds one network, sets what the network needs and
//! selects it. The supplicant does the 802.11 authentication, the 4-way handshake and the rekeys with the chip's
//! driver; this file only says which network and reports how it went. iOS decides when to join and leave: when the
//! link drops, the network is removed, so the supplicant does not come back on its own.
//! C: wifi/supplicant.h, wifi/supplicant.c.

use super::proto::*;
use crate::hal::aidl::{self, CallError, Connection, Parcel, Reader};
use crate::hal::binder::strerror;
use crate::{br_error, br_info};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

pub const SUP_SERVICE: &str = "android.hardware.wifi.supplicant.ISupplicant/default";
pub const SUP_MANAGER: &str = "android.os.IServiceManager";
pub const SUP_ISUPPLICANT: &str = "android.hardware.wifi.supplicant.ISupplicant";
pub const SUP_STA_IFACE: &str = "android.hardware.wifi.supplicant.ISupplicantStaIface";
pub const SUP_STA_NETWORK: &str = "android.hardware.wifi.supplicant.ISupplicantStaNetwork";
pub const SUP_STA_CALLBACK: &str = "android.hardware.wifi.supplicant.ISupplicantStaIfaceCallback";
/// The callback object's version: 2, the lowest common (the S24 Ultra's image serves 2, the base S25's 3; version 3
/// added callbacks after version 2's, and the service calls only the ones a callback of this version has).
pub const SUP_VERSION: i32 = 2;
pub const SUP_HASH: &str = "5e5c6d9e5565999f73fa7f797f5b94616174745b";

// Transaction codes: the place of each method in the frozen version 2 .aidl files (the first is 1). Version 3's
// methods come after these, so these codes are version 3's too (the base S25's image).
pub const SM_GET_SERVICE: u32 = 1; // IServiceManager.getService
pub const SUP_ADD_STA_INTERFACE: u32 = 2;
pub const SUP_GET_DEBUG_LEVEL: u32 = 3;
pub const SUP_GET_STA_INTERFACE: u32 = 5;
pub const SUP_REMOVE_INTERFACE: u32 = 10;
pub const STA_ADD_NETWORK: u32 = 3;
pub const STA_DISCONNECT: u32 = 6;
pub const STA_GET_MAC_ADDRESS: u32 = 15;
pub const STA_GET_NAME: u32 = 16;
pub const STA_LIST_NETWORKS: u32 = 26;
pub const STA_REGISTER_CALLBACK: u32 = 29;
pub const STA_REMOVE_NETWORK: u32 = 35;
pub const STA_GET_SIGNAL_POLL_RESULTS: u32 = 62;
pub const NET_GET_ID: u32 = 25;
pub const NET_SELECT: u32 = 45;
pub const NET_SET_GROUP_CIPHER: u32 = 72;
pub const NET_SET_KEY_MGMT: u32 = 75;
pub const NET_SET_PAIRWISE_CIPHER: u32 = 77;
pub const NET_SET_PROTO: u32 = 80;
pub const NET_SET_PSK: u32 = 81;
pub const NET_SET_PSK_PASSPHRASE: u32 = 82;
pub const NET_SET_REQUIRE_PMF: u32 = 83;
pub const NET_SET_SAE_H2E_MODE: u32 = 84;
pub const NET_SET_SAE_PASSWORD: u32 = 85;
pub const NET_SET_SCAN_SSID: u32 = 87;
pub const NET_SET_SSID: u32 = 88;
pub const CB_ASSOCIATION_REJECTED: u32 = 2;
pub const CB_AUTHENTICATION_TIMEOUT: u32 = 3;
pub const CB_DISCONNECTED: u32 = 7;
pub const CB_NETWORK_NOT_FOUND: u32 = 21;
pub const CB_STATE_CHANGED: u32 = 24;
pub const CB_BSS_FREQUENCY_CHANGED: u32 = 33;
pub const CB_SUPPLICANT_STATE_CHANGED: u32 = 34;
// KeyMgmtMask, ProtoMask, PairwiseCipherMask, GroupCipherMask
pub const KM_WPA_PSK: u32 = 1 << 1;
pub const KM_NONE: u32 = 1 << 2;
pub const KM_WPA_PSK_SHA256: u32 = 1 << 8;
pub const KM_SAE: u32 = 1 << 10;
pub const KM_OWE: u32 = 1 << 22;
pub const PROTO_WPA: u32 = 1 << 0;
pub const PROTO_RSN: u32 = 1 << 1;
pub const PC_TKIP: u32 = 1 << 3;
pub const PC_CCMP: u32 = 1 << 4;
pub const PC_GCMP_128: u32 = 1 << 6;
pub const PC_GCMP_256: u32 = 1 << 8;
pub const GC_TKIP: u32 = 1 << 3;
pub const GC_CCMP: u32 = 1 << 4;
pub const GC_GCMP_128: u32 = 1 << 6;
pub const GC_GCMP_256: u32 = 1 << 8;
// StaIfaceCallbackState
pub const ST_DISCONNECTED: i32 = 0;
pub const ST_IFACE_DISABLED: i32 = 1;
pub const ST_INACTIVE: i32 = 2;
pub const ST_SCANNING: i32 = 3;
pub const ST_AUTHENTICATING: i32 = 4;
pub const ST_ASSOCIATING: i32 = 5;
pub const ST_ASSOCIATED: i32 = 6;
pub const ST_FOURWAY_HANDSHAKE: i32 = 7;
pub const ST_GROUP_HANDSHAKE: i32 = 8;
pub const ST_COMPLETED: i32 = 9;

// ---- what a network asks for --------------------------------------------------------------------
/// What a BSS's information elements offer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Security {
    pub rsn: bool, // an RSN element
    pub wpa: bool, // a WPA (version 1) element
    pub psk: bool,
    pub psk_sha256: bool,
    pub sae: bool,
    pub owe: bool,
    pub eap: bool,
    pub tkip: bool, // pairwise ciphers
    pub ccmp: bool,
    pub gcmp: bool,
    pub pmf_capable: bool,
    pub pmf_required: bool,
}

fn suites(p: &[u8], wpa1: bool, out: &mut Security, akm: bool) {
    const RSN_OUI: [u8; 3] = [0x00, 0x0f, 0xac];
    const WPA_OUI: [u8; 3] = [0x00, 0x50, 0xf2];
    for s in p.chunks_exact(4) {
        if s[..3] != if wpa1 { WPA_OUI } else { RSN_OUI } {
            continue;
        }
        let t = s[3];
        if akm {
            match t {
                2 | 4 => out.psk = true, // PSK, FT-PSK
                6 => out.psk_sha256 = true,
                8 | 9 | 24 | 25 if !wpa1 => out.sae = true, // SAE, FT-SAE and their extended-key kinds
                18 if !wpa1 => out.owe = true,
                1 | 3 | 5 | 11 | 12 | 13 => out.eap = true,
                _ => {}
            }
        } else {
            match t {
                2 => out.tkip = true,
                4 => out.ccmp = true,
                8 | 9 if !wpa1 => out.gcmp = true,
                _ => {}
            }
        }
    }
}

/// version(2) group(4) pairwise count(2) + 4 each, AKM count(2) + 4 each, capabilities(2)
fn rsn_body(p: &[u8], wpa1: bool, out: &mut Security) {
    let n = p.len();
    if n < 8 {
        return;
    }
    let mut at = 6;
    let count = |at: usize| (p[at] as usize) | (p[at + 1] as usize) << 8;
    let c = count(at);
    at += 2;
    if at + 4 * c > n {
        return;
    }
    suites(&p[at..at + 4 * c], wpa1, out, false);
    at += 4 * c;
    if at + 2 > n {
        return;
    }
    let c = count(at);
    at += 2;
    if at + 4 * c > n {
        return;
    }
    suites(&p[at..at + 4 * c], wpa1, out, true);
    at += 4 * c;
    if wpa1 || at + 2 > n {
        return;
    }
    let caps = count(at);
    out.pmf_required = caps & 0x40 != 0;
    out.pmf_capable = caps & 0x80 != 0;
}

pub fn security_parse(ies: &[u8]) -> Security {
    let mut out = Security::default();
    let mut i = 0;
    while i + 2 <= ies.len() {
        let (id, len) = (ies[i], ies[i + 1] as usize);
        if i + 2 + len > ies.len() {
            break;
        }
        let body = &ies[i + 2..i + 2 + len];
        if id == 48 {
            out.rsn = true;
            rsn_body(body, false, &mut out);
        } else if id == 221 && len >= 4 && body[..4] == [0x00, 0x50, 0xf2, 1] {
            out.wpa = true;
            rsn_body(&body[4..], true, &mut out);
        }
        i += 2 + len;
    }
    out
}

/// The supplicant network of a join: the calls of ISupplicantStaNetwork to make, decided from what iOS gave (the key)
/// and what the last scan saw of the network.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub key_mgmt: u32,
    pub proto: u32,
    pub pairwise: u32,
    pub group: u32,
    pub psk_passphrase: bool, // which secret call takes the key
    pub psk_raw: bool,
    pub sae_password: bool,
    pub require_pmf: bool,
    pub set_pmf: bool,      // false: the supplicant's own default
    pub scan_ssid: bool,    // probe for the name: the network was in no scan
    pub kind: &'static str, // for the log: "open", "WPA2 (passphrase)", ...
}

/// `seen`: what the last scan saw of the network (None: not seen). -EINVAL when the key cannot open the network (a raw
/// WPA2 key for a WPA3-only network).
pub fn plan_join(join: &OwJoin, seen: Option<&Security>) -> Result<Plan, i32> {
    let mut plan = Plan { scan_ssid: seen.is_none(), ..Default::default() };
    let key = join.key_length != 0 && join.security != OW_SECURITY_OPEN;
    if !key {
        if seen.is_some_and(|s| s.owe && !s.psk && !s.sae) {
            // Enhanced Open
            plan.key_mgmt = KM_OWE;
            plan.proto = PROTO_RSN;
            plan.pairwise = PC_CCMP | PC_GCMP_256;
            plan.group = GC_CCMP | GC_GCMP_256;
            plan.set_pmf = true;
            plan.require_pmf = true;
            plan.kind = "enhanced open (OWE)";
        } else {
            plan.key_mgmt = KM_NONE;
            plan.kind = "open";
        }
        return Ok(plan);
    }
    plan.proto = PROTO_RSN;
    plan.pairwise = PC_CCMP | PC_GCMP_128 | PC_GCMP_256;
    plan.group = GC_CCMP | GC_GCMP_128 | GC_GCMP_256;
    let sae_only = seen.is_some_and(|s| s.sae && !s.psk && !s.psk_sha256);
    if join.key_raw != 0 {
        if join.key_length != 32 || sae_only || join.security == OW_SECURITY_SAE {
            return Err(-libc::EINVAL); // WPA3 needs the password itself
        }
        plan.psk_raw = true;
        plan.kind = "WPA2 (key)";
    } else if sae_only || join.security == OW_SECURITY_SAE {
        plan.key_mgmt = KM_SAE;
        plan.sae_password = true;
        plan.set_pmf = true;
        plan.require_pmf = true;
        plan.kind = "WPA3 (SAE)";
        return Ok(plan);
    } else {
        plan.psk_passphrase = true;
        plan.kind = if seen.is_some_and(|s| s.sae) {
            "WPA2 (passphrase; the network also offers WPA3)"
        } else {
            "WPA2 (passphrase)"
        };
    }
    plan.key_mgmt = KM_WPA_PSK | KM_WPA_PSK_SHA256;
    if seen.is_none_or(|s| s.wpa || s.tkip) {
        // an old network, or one not seen: WPA and TKIP allowed too
        plan.proto |= PROTO_WPA;
        plan.pairwise |= PC_TKIP;
        plan.group |= GC_TKIP;
    }
    Ok(plan)
}

// ---- the parcels --------------------------------------------------------------------------------
pub fn parcel_get_service(name: &str) -> Parcel {
    let mut p = Parcel::with_token(SUP_MANAGER);
    p.put_str(name);
    p
}
pub fn parcel_empty(descriptor: &str) -> Parcel {
    Parcel::with_token(descriptor)
}
pub fn parcel_string(descriptor: &str, text: &str) -> Parcel {
    let mut p = parcel_empty(descriptor);
    p.put_str(text);
    p
}
pub fn parcel_i32(descriptor: &str, value: i32) -> Parcel {
    let mut p = parcel_empty(descriptor);
    p.put_i32(value);
    p
}
pub fn parcel_bytes(descriptor: &str, bytes: &[u8]) -> Parcel {
    let mut p = parcel_empty(descriptor);
    p.put_bytes(bytes);
    p
}
pub fn parcel_callback(object: u64) -> Parcel {
    let mut p = parcel_empty(SUP_STA_IFACE);
    p.put_local(object);
    p
}

/// A callback of ISupplicantStaIfaceCallback, decoded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SupEvent {
    pub kind: u32, // a CB_* code, or 0: not one the bridge uses
    pub state: i32,
    pub id: i32,
    pub frequency: i32,
    pub code: u32, // disconnect: the reason; rejection: the status
    pub local: bool,
    pub timed_out: bool,
    pub bssid: [u8; 6],
}

fn bssid_of(r: &mut Reader<'_>, e: &mut SupEvent) {
    if let Some(b) = r.bytes().filter(|b| b.len() == 6) {
        e.bssid.copy_from_slice(b);
    }
}

pub fn decode_callback(code: u32, r: &mut Reader<'_>) -> SupEvent {
    let mut e = SupEvent::default();
    match code {
        CB_STATE_CHANGED => {
            // newState, bssid, id, ssid, filsHlpSent
            e.state = r.i32();
            bssid_of(r, &mut e);
            e.id = r.i32();
        }
        CB_SUPPLICANT_STATE_CHANGED => {
            // SupplicantStateChangeData: newState, id, ssid, bssid, keyMgmtMask, frequencyMhz, filsHlpSent
            if r.parcelable().is_none() {
                return e;
            }
            e.state = r.i32();
            e.id = r.i32();
            let _ssid = r.bytes();
            bssid_of(r, &mut e);
            let _key_mgmt = r.i32();
            e.frequency = r.i32();
        }
        CB_DISCONNECTED => {
            // bssid, locallyGenerated, reasonCode
            bssid_of(r, &mut e);
            e.local = r.i32() != 0;
            e.code = r.i32() as u32;
        }
        CB_ASSOCIATION_REJECTED => {
            // AssociationRejectionData: ssid, bssid, statusCode, timedOut, ...
            if r.parcelable().is_none() {
                return e;
            }
            let _ssid = r.bytes();
            bssid_of(r, &mut e);
            e.code = r.i32() as u32;
            e.timed_out = r.i32() != 0;
        }
        CB_AUTHENTICATION_TIMEOUT => bssid_of(r, &mut e), // bssid
        CB_NETWORK_NOT_FOUND => {}                        // ssid
        CB_BSS_FREQUENCY_CHANGED => e.frequency = r.i32(),
        _ => return e, // not one the bridge uses
    }
    if !r.is_bad() {
        e.kind = code;
    }
    e
}

// ---- the client ---------------------------------------------------------------------------------
/// What a join came to, or the supplicant's end. Called on the binder's looper thread: the receiver must not call
/// back into the supplicant from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Joined,
    /// status: an OW_JOIN_*; code: the 802.11 status or reason.
    JoinFailed {
        status: u32,
        code: u32,
    },
    Disconnected {
        code: u32,
    },
    Died,
}
pub type EventFn = Box<dyn Fn(Event) + Send + Sync>;

const CALLBACK_OBJECT: u64 = 0x4f57_434b; // "OWCB": our one object
const DEATH_COOKIE: u64 = 0x4f57_5350;

#[derive(Default)]
struct JoinState {
    dead: bool,
    joining: bool,
    joined: bool,
    keyed: bool,
    reached: i32, // the furthest state of this join
    not_found: u32,
    shown: u32,
    frequency: i32,
}

/// What the looper's callbacks share with the calls.
struct Shared {
    state: Mutex<JoinState>,
    events: Option<EventFn>,
    quiet: AtomicBool, // closing: no more events
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, JoinState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
    fn emit(&self, e: Event) {
        if let (Some(f), false) = (&self.events, self.quiet.load(Ordering::Relaxed)) {
            f(e)
        }
    }

    /// A callback of the supplicant (the looper thread; one-way calls, in order).
    fn callback(&self, e: &SupEvent) {
        const STATES: [&str; 10] = [
            "disconnected",
            "interface disabled",
            "inactive",
            "scanning",
            "authenticating",
            "associating",
            "associated",
            "4-way handshake",
            "group handshake",
            "completed",
        ];
        let mut event = None;
        {
            let mut s = self.lock();
            let show = s.shown < 200;
            if show {
                s.shown += 1;
            }
            match e.kind {
                CB_STATE_CHANGED | CB_SUPPLICANT_STATE_CHANGED => {
                    if e.frequency > 0 {
                        s.frequency = e.frequency;
                    }
                    s.reached = s.reached.max(e.state);
                    if e.state == ST_COMPLETED && s.joining {
                        s.joining = false;
                        s.joined = true;
                        event = Some(Event::Joined);
                    }
                    if show {
                        br_info!(
                            "wifi: supplicant: {}{}",
                            STATES.get(e.state as usize).copied().unwrap_or("state ?"),
                            if event.is_some() { ": joined" } else { "" }
                        );
                    }
                }
                CB_BSS_FREQUENCY_CHANGED => s.frequency = e.frequency,
                CB_DISCONNECTED => {
                    if s.joined {
                        s.joined = false;
                        event = Some(Event::Disconnected { code: e.code });
                    } else if s.joining && s.keyed && s.reached >= ST_FOURWAY_HANDSHAKE && !e.local {
                        // The access point ended the key handshake: with a pre-shared key that is a wrong key.
                        s.joining = false;
                        event = Some(Event::JoinFailed { status: OW_JOIN_AUTH, code: e.code });
                    }
                    if show {
                        br_info!(
                            "wifi: supplicant: disconnected (reason {}{}){}",
                            e.code,
                            if e.local { ", by the phone" } else { "" },
                            if matches!(event, Some(Event::JoinFailed { .. })) { ": the key was refused" } else { "" }
                        );
                    }
                }
                CB_ASSOCIATION_REJECTED => {
                    if s.joining && !e.timed_out {
                        s.joining = false;
                        let status = if s.keyed && matches!(e.code, 13..=15) { OW_JOIN_AUTH } else { OW_JOIN_REJECTED };
                        event = Some(Event::JoinFailed { status, code: e.code });
                    }
                    if show {
                        br_info!(
                            "wifi: supplicant: association rejected (status {}{})",
                            e.code,
                            if e.timed_out { ", timed out: it tries again" } else { "" }
                        );
                    }
                }
                CB_AUTHENTICATION_TIMEOUT => {
                    if s.joining && s.keyed && s.reached >= ST_FOURWAY_HANDSHAKE {
                        s.joining = false;
                        event = Some(Event::JoinFailed { status: OW_JOIN_AUTH, code: 15 });
                    }
                    if show {
                        br_info!("wifi: supplicant: authentication timed out");
                    }
                }
                CB_NETWORK_NOT_FOUND => {
                    if s.joining {
                        s.not_found += 1;
                        if s.not_found >= 2 {
                            s.joining = false;
                            event = Some(Event::JoinFailed { status: OW_JOIN_NO_NETWORK, code: 0 });
                        }
                    }
                    if show {
                        br_info!("wifi: supplicant: the network was not found in its scan");
                    }
                }
                _ => {}
            }
        }
        if let Some(e) = event {
            self.emit(e);
        }
    }

    fn incoming(&self, object: u64, code: u32, one_way: bool, r: &mut Reader<'_>) -> Result<Parcel, i32> {
        if object != CALLBACK_OBJECT {
            return Err(aidl::UNKNOWN_TRANSACTION);
        }
        if let Some(reply) = aidl::standard(code, SUP_STA_CALLBACK, SUP_VERSION, SUP_HASH) {
            return Ok(reply);
        }
        if code > 0x00ff_ff00 {
            return Err(aidl::UNKNOWN_TRANSACTION); // dump, shell command and the like
        }
        if !r.token(SUP_STA_CALLBACK) {
            return Err(aidl::BAD_TYPE);
        }
        let e = decode_callback(code, r);
        if e.kind != 0 {
            self.callback(&e);
        }
        let mut reply = Parcel::new();
        if !one_way {
            reply.put_ok();
        }
        Ok(reply)
    }

    fn death(&self, cookie: u64) {
        if cookie != DEATH_COOKIE {
            return;
        }
        {
            let mut s = self.lock();
            s.dead = true;
            s.joining = false;
            s.joined = false;
        }
        br_error!("wifi: supplicant: its process died (init starts it again on the next lookup)");
        self.emit(Event::Died);
    }
}

/// The C's why(): a few words for a failed call.
fn why(e: CallError) -> String {
    const CODES: [&str; 12] = [
        "success",
        "unknown failure",
        "invalid arguments",
        "invalid interface",
        "unknown interface",
        "the interface exists",
        "the interface is disabled",
        "not disconnected",
        "invalid network",
        "unknown network",
        "not supported",
        "a request is running",
    ];
    match -e.rc {
        libc::EREMOTEIO => match CODES.get(e.specific as usize).filter(|_| e.specific >= 0) {
            Some(c) => format!("the supplicant says: {c}"),
            None => format!("exception {}", e.specific),
        },
        libc::ETIMEDOUT => "no answer in 5 s".to_owned(),
        libc::EPIPE => "the supplicant's process died".to_owned(),
        _ => strerror(e.rc),
    }
}

/// The station interface's address, its networks' ids, the signal poll (linkId, currentRssiDbm, txBitrateMbps,
/// rxBitrateMbps, frequencyMhz; zeroes when not joined).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub mac: [u8; 6],
    pub networks: Vec<i32>,
    pub poll: [i32; 5],
}

pub struct Supplicant {
    conn: Connection,
    shared: Arc<Shared>,
    ifname: String,
    service: Option<u32>, // handles
    service_version: i32, // its getInterfaceVersion: 2 (S24 Ultra) or 3 (base S25)
    iface: Option<u32>,
    network: Option<u32>,
    added_iface: bool,
    network_id: i32,
}

impl Supplicant {
    /// Finds the service (started by init on the first lookup: up to wait_ms), takes the station interface (adds it
    /// when the supplicant has none of that name), registers the callback and removes the networks left in it.
    /// `events` None: a look only (the tool beside One UI): nothing is added, registered or removed. None when any
    /// step fails; the reason is logged.
    pub fn open(ifname: &str, wait_ms: u32, events: Option<EventFn>) -> Option<Supplicant> {
        let shared = Arc::new(Shared {
            state: Mutex::new(JoinState { dead: true, ..Default::default() }),
            events,
            quiet: AtomicBool::new(false),
        });
        let (a, b) = (shared.clone(), shared.clone());
        let conn = Connection::open(
            None,
            Some(Box::new(move |object, code, one_way, r: &mut Reader<'_>| a.incoming(object, code, one_way, r))),
            Some(Box::new(move |cookie| b.death(cookie))),
        )
        .inspect_err(|e| br_error!("wifi: supplicant: the binder device: {}", strerror(e.errno())))
        .ok()?;
        let name: String = if ifname.is_empty() { "wlan0".into() } else { ifname.chars().take(15).collect() };
        let mut s = Supplicant {
            conn,
            shared,
            ifname: name,
            service: None,
            service_version: 0,
            iface: None,
            network: None,
            added_iface: false,
            network_id: 0,
        };
        s.connect_service(wait_ms).ok()?;
        Some(s)
    }

    fn look_only(&self) -> bool {
        self.shared.events.is_none()
    }

    /// The service's reported interface version (0: it did not answer): 2 on the S24 Ultra's image, 3 on the base
    /// S25's, whose methods used here are version 2's.
    pub fn version(&self) -> i32 {
        self.service_version
    }

    /// A call whose failure is logged with `what` (None: not logged).
    fn call(&self, handle: u32, code: u32, p: &Parcel, what: Option<&str>) -> Result<(), CallError> {
        self.conn.call_void(handle, code, p).inspect_err(|e| {
            if let Some(what) = what {
                br_error!("wifi: supplicant: {what}: {}", why(*e))
            }
        })
    }

    /// A handle from a reply: ours, before the reply's buffer (which holds the call's) goes back.
    fn call_handle(&self, handle: u32, code: u32, p: &Parcel) -> Result<u32, CallError> {
        self.conn.call(handle, code, p, |r| aidl::take_handle(&self.conn, r))
    }

    fn remove_networks(&self, iface: u32) {
        let Ok(ids) =
            self.conn.call(iface, STA_LIST_NETWORKS, &parcel_empty(SUP_STA_IFACE), |r| r.i32s(32).ok_or(-libc::EPROTO))
        else {
            return;
        };
        for id in &ids {
            let _ = self.call(iface, STA_REMOVE_NETWORK, &parcel_i32(SUP_STA_IFACE, *id), Some("removeNetwork"));
        }
        if !ids.is_empty() {
            br_info!(
                "wifi: supplicant: {} network(s) of One UI's removed from it (the framework adds its own again when it starts)",
                ids.len()
            );
        }
    }

    /// The service, the interface, the callback.
    fn connect_service(&mut self, wait_ms: u32) -> Result<(), CallError> {
        let mut waited = 0;
        let found = loop {
            let rc = self.call_handle(0, SM_GET_SERVICE, &parcel_get_service(SUP_SERVICE));
            if !matches!(rc, Err(e) if e.rc == -libc::ENOENT) || waited >= wait_ms {
                break rc;
            }
            std::thread::sleep(Duration::from_millis(200)); // init is starting it
            waited += 200;
        };
        let service = found.inspect_err(|e| {
            br_error!(
                "wifi: supplicant: no {SUP_SERVICE} from the service manager: {}{}",
                why(*e),
                if e.rc == -libc::ENOENT { " (not registered, or SELinux denies find to this domain)" } else { "" }
            )
        })?;
        self.service = Some(service);
        let _ = self.conn.watch(service, DEATH_COOKIE);
        // Its frozen interface version, kept for `wifictl status` and the log: 2 on the S24 Ultra's image, 3 on the
        // base S25's. Every code used below is a version 2 one, and version 3 moved none of them.
        self.service_version = self
            .conn
            .call(service, aidl::GET_VERSION, &Parcel::with_token(SUP_ISUPPLICANT), aidl::take_i32)
            .unwrap_or(0);
        if self.service_version > SUP_VERSION {
            br_info!(
                "wifi: supplicant: ISupplicant v{}: using the v2 calls (v3 appended its methods, so the codes are its too)",
                self.service_version
            );
        }
        let mut iface = self.call_handle(service, SUP_GET_STA_INTERFACE, &parcel_string(SUP_ISUPPLICANT, &self.ifname));
        if iface.is_err() && !self.look_only() {
            // No station interface of that name: the supplicant started without its client (One UI's framework adds it).
            iface = self.call_handle(service, SUP_ADD_STA_INTERFACE, &parcel_string(SUP_ISUPPLICANT, &self.ifname));
            if iface.is_ok() {
                self.added_iface = true;
                br_info!("wifi: supplicant: it had no {}: added (removed again at the return)", self.ifname);
            }
        }
        let iface =
            iface.inspect_err(|e| br_error!("wifi: supplicant: no station interface {}: {}", self.ifname, why(*e)))?;
        self.iface = Some(iface);
        if self.look_only() {
            return Ok(());
        }
        self.call(
            iface,
            STA_REGISTER_CALLBACK,
            &parcel_callback(CALLBACK_OBJECT),
            Some(
                "registerCallback (can the supplicant call into this domain? look for avc denials \
                 with scontext=...hal_wifi_supplicant in dmesg)",
            ),
        )?;
        let _ = self.call(iface, STA_DISCONNECT, &parcel_empty(SUP_STA_IFACE), None);
        self.remove_networks(iface);
        self.shared.lock().dead = false;
        Ok(())
    }

    fn drop_handles(&mut self) {
        for h in [self.network.take(), self.iface.take(), self.service.take()].into_iter().flatten() {
            self.conn.release(h);
        }
    }

    fn is_dead(&self) -> bool {
        self.shared.lock().dead
    }

    fn forget_network(&mut self) {
        let (Some(network), Some(iface)) = (self.network, self.iface) else { return };
        let _ = self.call(iface, STA_REMOVE_NETWORK, &parcel_i32(SUP_STA_IFACE, self.network_id), None);
        self.conn.release(network);
        self.network = None;
    }

    /// Disconnects and removes the network. -ENODEV: the supplicant is gone.
    pub fn leave(&mut self) -> Result<(), i32> {
        let dead = {
            let mut s = self.shared.lock();
            s.joining = false;
            s.joined = false;
            s.dead
        };
        let Some(iface) = self.iface.filter(|_| !dead) else { return Err(-libc::ENODEV) };
        let rc = self.call(iface, STA_DISCONNECT, &parcel_empty(SUP_STA_IFACE), None);
        self.forget_network();
        rc.map_err(|e| e.rc)
    }

    /// One network: removes the last one, adds this, selects it. The result comes as an event.
    pub fn join(&mut self, join: &OwJoin, ies: Option<&[u8]>) -> Result<(), i32> {
        if self.is_dead() {
            // it died: init starts it again on this lookup
            self.drop_handles();
            self.added_iface = false;
            self.connect_service(5000).map_err(|e| e.rc)?;
        }
        let seen = ies.map(security_parse);
        let plan = plan_join(join, seen.as_ref()).inspect_err(|_| {
            br_error!("wifi: join: the network is WPA3 only and iOS gave a WPA2 key, not the password: not joined")
        })?;
        let _ = self.leave(); // one network at a time
        let iface = self.iface.ok_or(-libc::ENODEV)?;
        let n = self
            .call_handle(iface, STA_ADD_NETWORK, &parcel_empty(SUP_STA_IFACE))
            .inspect_err(|e| br_error!("wifi: supplicant: addNetwork: {}", why(*e)))
            .map_err(|e| e.rc)?;
        self.network = Some(n);
        let mut rc = self
            .conn
            .call(n, NET_GET_ID, &parcel_empty(SUP_STA_NETWORK), aidl::take_i32)
            .map(|id| self.network_id = id);
        let step = |rc: &mut Result<(), CallError>, code: u32, p: &mut Parcel, what: &str| {
            if rc.is_ok() {
                *rc = self.call(n, code, p, Some(what));
            }
        };
        step(&mut rc, NET_SET_SSID, &mut parcel_bytes(SUP_STA_NETWORK, join.ssid()), "setSsid");
        step(&mut rc, NET_SET_KEY_MGMT, &mut parcel_i32(SUP_STA_NETWORK, plan.key_mgmt as i32), "setKeyMgmt");
        if plan.proto != 0 {
            step(&mut rc, NET_SET_PROTO, &mut parcel_i32(SUP_STA_NETWORK, plan.proto as i32), "setProto");
        }
        if plan.pairwise != 0 {
            step(
                &mut rc,
                NET_SET_PAIRWISE_CIPHER,
                &mut parcel_i32(SUP_STA_NETWORK, plan.pairwise as i32),
                "setPairwiseCipher",
            );
        }
        if plan.group != 0 {
            step(&mut rc, NET_SET_GROUP_CIPHER, &mut parcel_i32(SUP_STA_NETWORK, plan.group as i32), "setGroupCipher");
        }
        if plan.set_pmf {
            step(
                &mut rc,
                NET_SET_REQUIRE_PMF,
                &mut parcel_i32(SUP_STA_NETWORK, i32::from(plan.require_pmf)),
                "setRequirePmf",
            );
        }
        if plan.scan_ssid {
            step(&mut rc, NET_SET_SCAN_SSID, &mut parcel_i32(SUP_STA_NETWORK, 1), "setScanSsid");
        }
        if rc.is_ok() && (plan.psk_passphrase || plan.sae_password) {
            let text = String::from_utf8_lossy(join.key()).into_owned();
            let mut p = parcel_string(SUP_STA_NETWORK, &text);
            let (code, what) = if plan.sae_password {
                (NET_SET_SAE_PASSWORD, "setSaePassword")
            } else {
                (NET_SET_PSK_PASSPHRASE, "setPskPassphrase")
            };
            step(&mut rc, code, &mut p, what);
            p.wipe();
            let mut text = text.into_bytes();
            text.fill(0);
        }
        if rc.is_ok() && plan.sae_password {
            // hash-to-element where the access point asks for it; an older supplicant has no such call
            let _ = self.call(n, NET_SET_SAE_H2E_MODE, &parcel_i32(SUP_STA_NETWORK, 1 /* H2E_OPTIONAL */), None);
        }
        if rc.is_ok() && plan.psk_raw {
            let mut p = parcel_bytes(SUP_STA_NETWORK, &join.key[..32]);
            step(&mut rc, NET_SET_PSK, &mut p, "setPsk");
            p.wipe();
        }
        if rc.is_ok() {
            {
                let mut s = self.shared.lock();
                s.joining = true;
                s.joined = false;
                s.keyed = plan.key_mgmt != KM_NONE && plan.key_mgmt != KM_OWE;
                s.reached = 0;
                s.not_found = 0;
            }
            rc = self.call(n, NET_SELECT, &parcel_empty(SUP_STA_NETWORK), Some("select"));
        }
        if let Err(e) = rc {
            self.shared.lock().joining = false;
            self.forget_network();
            return Err(e.rc);
        }
        br_info!(
            "wifi: join: network {} in the supplicant, {}{}: selected",
            self.network_id,
            plan.kind,
            if plan.scan_ssid { ", not in the last scan" } else { "" }
        );
        Ok(())
    }

    pub fn frequency(&self) -> u32 {
        self.shared.lock().frequency.max(0) as u32
    }

    /// The interface's address, its networks' ids (at most `max`) and the signal poll; for the tool.
    pub fn status(&self, max: usize) -> Result<Status, i32> {
        let iface = self.iface.ok_or(-libc::ENODEV)?;
        let mac = self
            .conn
            .call(iface, STA_GET_MAC_ADDRESS, &parcel_empty(SUP_STA_IFACE), |r| {
                r.bytes()
                    .filter(|b| b.len() == 6)
                    .map(|b| <[u8; 6]>::try_from(b).expect("6 bytes"))
                    .ok_or(-libc::EPROTO)
            })
            .inspect_err(|e| br_error!("wifi: supplicant: getMacAddress: {}", why(*e)))
            .map_err(|e| e.rc)?;
        let networks = self
            .conn
            .call(iface, STA_LIST_NETWORKS, &parcel_empty(SUP_STA_IFACE), |r| r.i32s(max).ok_or(-libc::EPROTO))
            .inspect_err(|e| br_error!("wifi: supplicant: listNetworks: {}", why(*e)))
            .map_err(|e| e.rc)?;
        // SignalPollResult[]: linkId, currentRssiDbm, txBitrateMbps, rxBitrateMbps, frequencyMhz; fails while not joined
        let poll = self
            .conn
            .call(iface, STA_GET_SIGNAL_POLL_RESULTS, &parcel_empty(SUP_STA_IFACE), |r| {
                if r.i32() < 1 || r.is_bad() {
                    return Err(-libc::ENOENT);
                }
                r.parcelable().ok_or(-libc::EPROTO)?;
                let v: [i32; 5] = std::array::from_fn(|_| r.i32());
                if r.is_bad() {
                    Err(-libc::EPROTO)
                } else {
                    Ok(v)
                }
            })
            .unwrap_or_default();
        Ok(Status { mac, networks, poll })
    }
}

impl Drop for Supplicant {
    /// Leaves the network, removes the interface if open added it, and closes the binder (the supplicant drops a
    /// dead client's callback).
    fn drop(&mut self) {
        if !self.look_only() && !self.is_dead() {
            let _ = self.leave();
            if let (true, Some(service)) = (self.added_iface, self.service) {
                // IfaceInfo: type (STA 0), name
                let mut p = parcel_empty(SUP_ISUPPLICANT);
                p.put_parcelable(|p| {
                    p.put_i32(0);
                    p.put_str(&self.ifname);
                });
                let _ = self.call(service, SUP_REMOVE_INTERFACE, &p, Some("removeInterface"));
            }
        }
        self.shared.quiet.store(true, Ordering::Relaxed);
        self.drop_handles();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hal::aidl::{FlatObject, Object, TYPE_HANDLE};
    use crate::hal::binder::offsets_bytes;

    // test_wifi.c test_parcels: the AIDL parcels byte for byte.
    #[test]
    fn parcels() {
        let p = parcel_get_service("ab/c");
        let token = [
            0u8, 0, 0, 0x80, 0xff, 0xff, 0xff, 0xff, b'T', b'S', b'Y', b'S', 26, 0, 0, 0, b'a', 0, b'n', 0, b'd', 0,
            b'r', 0,
        ];
        assert!(!p.is_overflow() && p.object_offsets().is_empty() && p.bytes()[..token.len()] == token);
        // "android.os.IServiceManager": 26 units and a terminator, 54 bytes padded to 56; then "ab/c": 4 units, 10 bytes padded to 12
        let name = [4u8, 0, 0, 0, b'a', 0, b'b', 0, b'/', 0, b'c', 0, 0, 0, 0, 0];
        assert!(p.size() == 12 + 4 + 56 + 16 && p.bytes()[72..88] == name);
        let mut r = p.reader();
        assert!(
            r.token(SUP_MANAGER) && r.string(63).as_deref() == Some("ab/c") && !r.is_bad() && r.position() == p.size()
        );
        assert!(!p.reader().token(SUP_ISUPPLICANT));

        // A callback object: the kernel's object, listed, then the stability word.
        let p = parcel_callback(0x1122_3344_5566_7788);
        let at = 12 + 4 + (((SUP_STA_IFACE.len() + 1) * 2 + 3) & !3);
        let object = [
            0x85u8, 0x2a, 0x62, 0x73, 0x13, 0x01, 0, 0, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x88, 0x77,
            0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x3f, 0, 0, 0,
        ];
        assert!(p.object_offsets() == [at as u64] && p.size() == at + 28 && p.bytes()[at..at + 28] == object);
        // As its receiver sees it: a handle. Not listed in the offsets: refused.
        let mut copy = p.bytes().to_vec();
        let h = FlatObject { kind: TYPE_HANDLE, flags: 0x113, binder_or_handle: 7, cookie: 0 }.to_bytes();
        copy[at..at + 4].copy_from_slice(&h[..4]);
        copy[at + 8..at + 24].copy_from_slice(&h[8..24]);
        let mut r = Reader::new(&copy, offsets_bytes(p.object_offsets()));
        assert!(r.token(SUP_STA_IFACE) && r.binder() == Object::Handle(7) && !r.is_bad());
        let mut r = Reader::new(&copy, &[]);
        assert!(r.token(SUP_STA_IFACE) && r.binder() == Object::Null && r.is_bad());
        // A null binder: not listed, not an error.
        let mut p = Parcel::new();
        p.put_ok();
        p.put_null_binder();
        let mut r = p.reader();
        assert!(p.object_offsets().is_empty() && r.status().is_ok() && r.binder() == Object::Null && !r.is_bad());

        // byte[] and int, a key wiped.
        let mut p = parcel_bytes(SUP_STA_NETWORK, b"secret-key");
        let at = p.size() - 16;
        assert!(p.bytes()[at] == 10 && p.bytes()[at + 4..at + 16] == *b"secret-key\0\0");
        p.wipe();
        assert!(p.size() == 0 && !p.bytes().contains(&b's'));
        let p = parcel_i32(SUP_STA_NETWORK, (KM_WPA_PSK | KM_WPA_PSK_SHA256) as i32);
        assert!(p.bytes()[p.size() - 4] == 0x02 && p.bytes()[p.size() - 3] == 0x01);

        // A reply's status: ok; a service's error; an exception with a stack trace header; a truncated one.
        let mut p = Parcel::new();
        p.put_service_error(4);
        p.put_i32(77);
        let mut r = p.reader();
        assert!(r.status() == Err((aidl::EX_SERVICE_SPECIFIC, 4)) && r.i32() == 77 && !r.is_bad());
        let mut p = Parcel::new();
        p.put_i32(-3);
        p.put_str("bad");
        for v in [12, 1, 2, 99] {
            p.put_i32(v);
        }
        let mut r = p.reader();
        assert!(r.status() == Err((-3, -3)) && r.i32() == 99);
        let mut r = Reader::new(&p.bytes()[..6], &[]);
        let _ = r.status();
        assert!(r.is_bad());
        // Strings: a null one, one cut to the buffer, one whose length is past the parcel.
        let mut p = Parcel::new();
        p.put_string(None);
        p.put_str("abcdefgh");
        p.put_i32(1000);
        let mut r = p.reader();
        assert!(
            r.string(63).as_deref() == Some("")
                && r.string(4).as_deref() == Some("abcd")
                && r.string(63).is_none()
                && r.is_bad()
        );
        // The standard answers.
        let p = aidl::standard(aidl::INTERFACE, SUP_STA_CALLBACK, 2, SUP_HASH).unwrap();
        assert_eq!(p.bytes()[0] as usize, SUP_STA_CALLBACK.len());
        let p = aidl::standard(aidl::GET_VERSION, SUP_STA_CALLBACK, 2, SUP_HASH).unwrap();
        assert!(p.size() == 8 && p.bytes()[4] == 2);
        let p = aidl::standard(aidl::GET_HASH, SUP_STA_CALLBACK, 2, SUP_HASH).unwrap();
        assert!(p.bytes()[4] == 40 && aidl::standard(aidl::PING, "", 0, "").unwrap().size() == 0);
        assert!(aidl::standard(CB_DISCONNECTED, SUP_STA_CALLBACK, 2, SUP_HASH).is_none());
        // An overflow is kept.
        let mut p = Parcel::new();
        p.put_bytes(&[0; 5000]);
        p.put_i32(1);
        assert!(p.is_overflow());
    }

    fn rsn(pairwise: &[u8], akm: &[u8], caps: u16) -> Vec<u8> {
        let mut o = vec![48, 0, 1, 0, 0x00, 0x0f, 0xac, 0x04, pairwise.len() as u8, 0];
        for p in pairwise {
            o.extend_from_slice(&[0x00, 0x0f, 0xac, *p]);
        }
        o.extend_from_slice(&[akm.len() as u8, 0]);
        for a in akm {
            o.extend_from_slice(&[0x00, 0x0f, 0xac, *a]);
        }
        o.extend_from_slice(&caps.to_le_bytes());
        o[1] = (o.len() - 2) as u8;
        o
    }

    // test_wifi.c test_security: what a network asks for
    #[test]
    fn security_and_plans() {
        let home = b"\x00\x04home".to_vec();
        let ies = |e: &[u8]| [&home[..], e].concat();
        let pass = OwJoin::new(b"home", OW_SECURITY_AUTO, b"password", false);
        let mut raw = OwJoin::new(b"home", OW_SECURITY_AUTO, &[0; 32], true);
        raw.key_length = 32;
        let open = OwJoin::new(b"home", OW_SECURITY_OPEN, b"", false);
        // WPA2: CCMP, PSK.
        let s = security_parse(&ies(&rsn(&[4], &[2], 0x000c)));
        assert!(s.rsn && s.psk && s.ccmp && !s.sae && !s.wpa && !s.tkip && !s.pmf_capable && !s.pmf_required);
        let plan = plan_join(&pass, Some(&s)).unwrap();
        assert!(plan.key_mgmt == KM_WPA_PSK | KM_WPA_PSK_SHA256 && plan.proto == PROTO_RSN && plan.psk_passphrase);
        assert!(
            !plan.psk_raw
                && !plan.sae_password
                && !plan.set_pmf
                && !plan.scan_ssid
                && plan.pairwise == PC_CCMP | PC_GCMP_128 | PC_GCMP_256
        );
        let plan = plan_join(&raw, Some(&s)).unwrap();
        assert!(plan.psk_raw && !plan.psk_passphrase && plan.key_mgmt == KM_WPA_PSK | KM_WPA_PSK_SHA256);
        // WPA3 only: SAE, management frame protection required.
        let s = security_parse(&ies(&rsn(&[4], &[8], 0x00cc)));
        assert!(s.sae && !s.psk && s.pmf_capable && s.pmf_required);
        let plan = plan_join(&pass, Some(&s)).unwrap();
        assert!(
            plan.key_mgmt == KM_SAE && plan.sae_password && plan.set_pmf && plan.require_pmf && !plan.psk_passphrase
        );
        assert_eq!(plan_join(&raw, Some(&s)), Err(-libc::EINVAL)); // a WPA2 key cannot open it
                                                                   // WPA2/WPA3 transition: the passphrase as WPA2.
        let s = security_parse(&ies(&rsn(&[4], &[2, 8], 0x008c)));
        assert!(s.sae && s.psk && s.pmf_capable && !s.pmf_required);
        let plan = plan_join(&pass, Some(&s)).unwrap();
        assert!(plan.psk_passphrase && plan.key_mgmt == KM_WPA_PSK | KM_WPA_PSK_SHA256 && !plan.set_pmf);
        // WPA (version 1) with TKIP beside WPA2: both protocols and TKIP allowed.
        let wpa1 = [
            221u8, 22, 0x00, 0x50, 0xf2, 1, 1, 0, 0x00, 0x50, 0xf2, 2, 1, 0, 0x00, 0x50, 0xf2, 2, 1, 0, 0x00, 0x50,
            0xf2, 2,
        ];
        let s = security_parse(&[ies(&rsn(&[2, 4], &[2], 0)), wpa1.to_vec()].concat());
        assert!(s.rsn && s.wpa && s.tkip && s.ccmp && s.psk);
        let plan = plan_join(&pass, Some(&s)).unwrap();
        assert!(plan.proto == PROTO_RSN | PROTO_WPA && plan.pairwise & PC_TKIP != 0 && plan.group & GC_TKIP != 0);
        // Open; enhanced open; no record of the network (hidden).
        let s = security_parse(&home);
        let plan = plan_join(&open, Some(&s)).unwrap();
        assert!(!s.rsn && !s.wpa && plan.key_mgmt == KM_NONE && plan.proto == 0 && !plan.scan_ssid);
        let s = security_parse(&ies(&rsn(&[4], &[18], 0x00cc)));
        let plan = plan_join(&open, Some(&s)).unwrap();
        assert!(s.owe && plan.key_mgmt == KM_OWE && plan.require_pmf);
        let plan = plan_join(&pass, None).unwrap();
        assert!(plan.scan_ssid && plan.psk_passphrase && plan.proto & PROTO_WPA != 0);
        let plan = plan_join(&open, None).unwrap();
        assert!(plan.scan_ssid && plan.key_mgmt == KM_NONE);
        // Elements cut short or with lengths past the end: nothing read past them.
        let mut e = ies(&rsn(&[4], &[2], 0x000c));
        for cut in 0..e.len() {
            security_parse(&e[..cut]);
        }
        e[6 + 2 + 6] = 200; // a pairwise count past the element
        let s = security_parse(&e);
        assert!(s.rsn && !s.psk && !s.ccmp);
    }

    // test_wifi.c test_callbacks: the supplicant's callbacks decoded
    #[test]
    fn callbacks() {
        let bssid = [1u8, 2, 3, 4, 5, 6];
        // onStateChanged(newState, bssid, id, ssid, filsHlpSent)
        let mut p = Parcel::with_token(SUP_STA_CALLBACK);
        p.put_i32(ST_FOURWAY_HANDSHAKE);
        p.put_bytes(&bssid);
        p.put_i32(3);
        p.put_bytes(b"home");
        p.put_i32(0);
        let mut r = p.reader();
        assert!(r.token(SUP_STA_CALLBACK));
        let e = decode_callback(CB_STATE_CHANGED, &mut r);
        assert!(e.kind == CB_STATE_CHANGED && e.state == ST_FOURWAY_HANDSHAKE && e.id == 3 && e.bssid == bssid);
        // onSupplicantStateChanged(SupplicantStateChangeData)
        let mut p = Parcel::new();
        p.put_parcelable(|p| {
            p.put_i32(ST_COMPLETED);
            p.put_i32(3);
            p.put_bytes(b"home");
            p.put_bytes(&bssid);
            p.put_i32(KM_WPA_PSK as i32);
            p.put_i32(5180);
            p.put_i32(0);
        });
        let e = decode_callback(CB_SUPPLICANT_STATE_CHANGED, &mut p.reader());
        assert!(
            e.kind == CB_SUPPLICANT_STATE_CHANGED
                && e.state == ST_COMPLETED
                && e.frequency == 5180
                && e.id == 3
                && e.bssid == bssid
        );
        let e = decode_callback(CB_SUPPLICANT_STATE_CHANGED, &mut Reader::new(&p.bytes()[..p.size() - 8], &[])); // cut short: not used
        assert_eq!(e.kind, 0);
        // onDisconnected(bssid, locallyGenerated, reasonCode)
        let mut p = Parcel::new();
        p.put_bytes(&bssid);
        p.put_i32(1);
        p.put_i32(3);
        let e = decode_callback(CB_DISCONNECTED, &mut p.reader());
        assert!(e.kind == CB_DISCONNECTED && e.local && e.code == 3);
        // onAssociationRejected(AssociationRejectionData: ssid, bssid, statusCode, timedOut, ...)
        let mut p = Parcel::new();
        p.put_parcelable(|p| {
            p.put_bytes(b"home");
            p.put_bytes(&bssid);
            for v in [17, 0, 0, 0, 0] {
                p.put_i32(v);
            }
        });
        let mut r = p.reader();
        let e = decode_callback(CB_ASSOCIATION_REJECTED, &mut r);
        assert!(e.kind == CB_ASSOCIATION_REJECTED && e.code == 17 && !e.timed_out);
        // One the bridge does not use.
        assert_eq!(decode_callback(20, &mut r).kind, 0);
    }
}
