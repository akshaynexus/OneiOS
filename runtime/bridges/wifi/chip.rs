// SPDX-License-Identifier: MIT
//! The Wi-Fi chip's HAL, for a session that starts with One UI's Wi-Fi switched off (docs/hardware.md section 3,
//! "Wi-Fi off in One UI at the switch"): then init never started the vendor's Wi-Fi stack, the chip is down and wlan0
//! does not exist, and the bridge used to say so and give iOS no network device. This starts the chip itself through
//! the same HAL the framework and wificond use, so the rest of the bridge finds its wlan0 as usual.
//!
//! android.hardware.wifi.IWifi/default, stable AIDL version 1 (the phone's VINTF manifest names V1;
//! `android.hardware.wifi-service`, in the vendor domain). What a session does, as the framework does:
//!   1. IWifi.isStarted(); when false, IWifi.start() (the HAL loads the driver and the firmware, which takes seconds)
//!   2. wait for the station interface the driver registers (wlan0) -- the driver creates it at load on this phone
//!   3. when the chip has no station interface at all, IWifiChip.createStaIface() and wait again
//!   4. at the return, IWifi.stop() -- but only when this session started it, so One UI's own Wi-Fi is left alone
//!
//! The codes are the place of each method in the frozen version 1 .aidl files (the first is 1); a newer version of
//! the interface only appends its methods, so these stay its own. The descriptors and the parcels are checked byte
//! for byte in the tests, and the flow against a fake HAL in another process on a real binder driver
//! (`tests/test_aidl_binder.rs`). Not run on the phone yet: the SELinux domain must be allowed to call
//! hal_wifi_default (a denial is logged and the session carries on as before this file).
//! C: wifi/chip.h, wifi/chip.c.

use crate::hal::aidl::{self, CallError, Connection, Parcel, Reader};
use crate::hal::binder::strerror;
use crate::{br_error, br_info};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

pub const WIFI_SERVICE: &str = "android.hardware.wifi.IWifi/default";
pub const WIFI_MANAGER: &str = "android.os.IServiceManager";
pub const WIFI_IWIFI: &str = "android.hardware.wifi.IWifi";
pub const WIFI_ICHIP: &str = "android.hardware.wifi.IWifiChip";
pub const WIFI_STA_IFACE: &str = "android.hardware.wifi.IWifiStaIface";
/// The version this file speaks: the frozen interface's 1. A newer service only appends methods after these.
pub const WIFI_VERSION: i32 = 1;

// Transaction codes of the frozen version 1 .aidl files, in their order.
pub const SM_GET_SERVICE: u32 = 1; // IServiceManager.getService
pub const WIFI_GET_CHIP: u32 = 1; // IWifi.getChip(int)
pub const WIFI_IS_STARTED: u32 = 3; // IWifi.isStarted()
pub const WIFI_START: u32 = 5; // IWifi.start()
pub const WIFI_STOP: u32 = 6; // IWifi.stop()
pub const CHIP_CREATE_STA_IFACE: u32 = 7; // IWifiChip.createStaIface()
pub const CHIP_GET_STA_IFACE_NAMES: u32 = 24; // IWifiChip.getStaIfaceNames()

/// How often the wait looks again: the netdev appears well within this of the driver's load.
const POLL_MS: u64 = 200;
/// A whole start's budget: the driver's load and the firmware's download on the phone.
pub const START_MS: u32 = 8000;
/// After createStaIface: the netdev's registration, the same order of time.
const CREATE_MS: u32 = 3000;

/// The chip this session started (or adopted): the stop uses the same service and knows whose it was.
struct Chip {
    conn: Connection,
    service: u32,
    started: bool, // this session started it: the stop stops it again
}

static CHIP: Mutex<Option<Chip>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<Chip>> {
    CHIP.lock().unwrap_or_else(PoisonError::into_inner)
}

fn parcel_empty(descriptor: &str) -> Parcel {
    Parcel::with_token(descriptor)
}

fn parcel_i32(descriptor: &str, value: i32) -> Parcel {
    let mut p = parcel_empty(descriptor);
    p.put_i32(value);
    p
}

fn parcel_string(descriptor: &str, text: &str) -> Parcel {
    let mut p = parcel_empty(descriptor);
    p.put_str(text);
    p
}

/// What a failed call means for the log. The supplicant's FAILED_* codes are not the HAL's (this interface answers
/// with binder's own statuses), so only the errno is named.
fn why(e: CallError) -> String {
    match -e.rc {
        libc::ENOENT => "not registered (or SELinux denies find to this domain)".to_owned(),
        libc::EPIPE => "its process died".to_owned(),
        libc::ENOSYS => "the HAL does not have that method".to_owned(),
        _ => strerror(e.rc).to_owned(),
    }
}

/// A String[] as getStaIfaceNames answers it: the count (or -1 for null), then each string.
fn sta_names(r: &mut Reader<'_>) -> Result<Vec<String>, i32> {
    let count = r.i32();
    if r.is_bad() {
        return Err(-libc::EPROTO);
    }
    if count == -1 {
        return Ok(Vec::new());
    }
    if !(0..=16).contains(&count) {
        return Err(-libc::EPROTO);
    }
    let mut names = Vec::with_capacity(count as usize);
    for _ in 0..count {
        match r.string(32) {
            Some(name) => names.push(name),
            None => return Err(-libc::EPROTO),
        }
    }
    Ok(names)
}

/// The service, with a lazy start's wait: init starts the HAL on the first lookup, so ENOENT is retried.
fn find(conn: &Connection, wait_ms: u32) -> Result<u32, CallError> {
    let mut waited = 0;
    loop {
        let rc =
            conn.call(0, SM_GET_SERVICE, &parcel_string(WIFI_MANAGER, WIFI_SERVICE), |r| aidl::take_handle(conn, r));
        match rc {
            Err(e) if e.rc == -libc::ENOENT && waited < wait_ms => {}
            other => return other,
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
        waited += POLL_MS as u32;
    }
}

/// The chip's state and station interfaces, without changing anything: `wifictl chip` beside One UI and the log.
pub struct Status {
    pub version: i32,
    pub started: bool,
    pub interfaces: Vec<String>,
}

impl Status {
    pub fn text(&self) -> String {
        format!(
            "IWifi v{}: {}, station interface(s): {}",
            self.version,
            if self.started { "started" } else { "stopped" },
            if self.interfaces.is_empty() { "none".to_owned() } else { self.interfaces.join(", ") }
        )
    }
}

/// One connection to the HAL and its service, released by the caller.
fn attach(wait_ms: u32) -> Result<(Connection, u32), String> {
    let conn = Connection::open(None, None, None).map_err(|e| format!("the binder device: {}", strerror(e.errno())))?;
    let service =
        find(&conn, wait_ms).map_err(|e| format!("no {WIFI_SERVICE} from the service manager: {}", why(e)))?;
    Ok((conn, service))
}

fn version(conn: &Connection, service: u32) -> Result<i32, String> {
    let v = conn
        .call(service, aidl::GET_VERSION, &parcel_empty(WIFI_IWIFI), aidl::take_i32)
        .map_err(|e| format!("IWifi.getInterfaceVersion: {}", why(e)))?;
    if v < WIFI_VERSION {
        return Err(format!("the HAL serves IWifi v{v}, older than the v{WIFI_VERSION} this bridge speaks"));
    }
    if v > WIFI_VERSION {
        br_info!("wifi: the Wi-Fi HAL serves IWifi v{v}: the v{WIFI_VERSION} calls come first in it");
    }
    Ok(v)
}

fn is_started(conn: &Connection, service: u32) -> Result<bool, String> {
    let v = conn
        .call(service, WIFI_IS_STARTED, &parcel_empty(WIFI_IWIFI), aidl::take_i32)
        .map_err(|e| format!("IWifi.isStarted: {}", why(e)))?;
    Ok(v != 0)
}

/// The chip's station interfaces (a failed getChip or the names read as none, with the reason logged).
fn interfaces(conn: &Connection, service: u32) -> Vec<String> {
    let chip = match conn.call(service, WIFI_GET_CHIP, &parcel_i32(WIFI_IWIFI, 0), |r| aidl::take_handle(conn, r)) {
        Ok(chip) => chip,
        Err(e) => {
            br_error!("wifi: the Wi-Fi HAL: IWifi.getChip(0): {}", why(e));
            return Vec::new();
        }
    };
    let names = conn.call(chip, CHIP_GET_STA_IFACE_NAMES, &parcel_empty(WIFI_ICHIP), sta_names);
    conn.release(chip);
    match names {
        Ok(names) => names,
        Err(e) => {
            br_error!("wifi: the Wi-Fi HAL: IWifiChip.getStaIfaceNames: {}", why(e));
            Vec::new()
        }
    }
}

/// Looks, changes nothing: the HAL's version, whether the chip runs, its station interfaces. None with the reason
/// logged.
pub fn status(wait_ms: u32) -> Option<Status> {
    let (conn, service) = match attach(wait_ms) {
        Ok(v) => v,
        Err(e) => {
            br_error!("wifi: {e}");
            return None;
        }
    };
    let looked: Result<Status, String> = (|| {
        let version = version(&conn, service)?;
        let started = is_started(&conn, service)?;
        Ok(Status { version, started, interfaces: interfaces(&conn, service) })
    })();
    conn.release(service);
    match looked {
        Ok(status) => Some(status),
        Err(e) => {
            br_error!("wifi: {e}");
            None
        }
    }
}

/// Starts the chip if it is not started, and waits for `name` to exist (up to `wait_ms`), creating the chip's station
/// interface when it has none. True: `name` is there. The chip is kept for [`give_back`] when this started it.
pub fn ensure(name: &str, wait_ms: u32, present: impl Fn() -> bool) -> bool {
    let mut slot = lock();
    if let Some(chip) = slot.as_ref() {
        // A second look in the same session: the chip is already this session's.
        return chip.started || present();
    }
    let (conn, service) = match attach(wait_ms.min(START_MS)) {
        Ok(v) => v,
        Err(e) => {
            br_error!("wifi: no {name}: {e}");
            return false;
        }
    };
    let started = match (version(&conn, service), is_started(&conn, service)) {
        (Ok(_), Ok(started)) => started,
        (Err(e), _) | (_, Err(e)) => {
            br_error!("wifi: no {name}: {e}");
            conn.release(service);
            return false;
        }
    };
    let mut ours = false;
    if !started {
        br_info!(
            "wifi: no {name}: the Wi-Fi HAL is not started (One UI's Wi-Fi was off at the switch?): starting the chip"
        );
        match conn.call_void(service, WIFI_START, &parcel_empty(WIFI_IWIFI)) {
            Ok(()) => ours = true,
            Err(e) => {
                br_error!("wifi: no {name}: IWifi.start: {}", why(e));
                conn.release(service);
                return false;
            }
        }
    } else {
        br_info!("wifi: no {name}: the Wi-Fi HAL says the chip is started; waiting for the interface");
    }
    let at = Instant::now();
    let mut there = wait_present(&present, wait_ms);
    if !there {
        // Started, but no netdev: the chip may have no station interface at all (one is created, as wificond does).
        let names = interfaces(&conn, service);
        if names.is_empty() {
            if create_sta(&conn, service) {
                there = wait_present(&present, if ours { CREATE_MS } else { wait_ms });
            }
        } else {
            br_info!(
                "wifi: the chip has station interface(s) {} but no {name}: not creating another",
                names.join(", ")
            );
        }
    }
    if !there {
        br_error!("wifi: the chip: still no {name} after {:.1} s: iOS gets no Wi-Fi", at.elapsed().as_secs_f64());
        if ours {
            let _ = conn.call_void(service, WIFI_STOP, &parcel_empty(WIFI_IWIFI));
        }
        conn.release(service);
        return false;
    }
    br_info!(
        "wifi: {}the chip; {name} appeared after {:.1} s",
        if ours { "started " } else { "adopted " },
        at.elapsed().as_secs_f64()
    );
    *slot = Some(Chip { conn, service, started: ours });
    true
}

/// One wait for the netdev, POLL_MS apart, the first look at once.
fn wait_present(present: &impl Fn() -> bool, wait_ms: u32) -> bool {
    let until = Instant::now() + Duration::from_millis(u64::from(wait_ms));
    loop {
        if present() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
}

/// IWifiChip.createStaIface(): the chip had none. True when the HAL created one.
fn create_sta(conn: &Connection, service: u32) -> bool {
    let chip = match conn.call(service, WIFI_GET_CHIP, &parcel_i32(WIFI_IWIFI, 0), |r| aidl::take_handle(conn, r)) {
        Ok(chip) => chip,
        Err(e) => {
            br_error!("wifi: the Wi-Fi HAL: IWifi.getChip(0): {}", why(e));
            return false;
        }
    };
    let made = conn.call(chip, CHIP_CREATE_STA_IFACE, &parcel_empty(WIFI_ICHIP), |r| aidl::take_handle(conn, r));
    conn.release(chip);
    match made {
        Ok(iface) => {
            conn.release(iface);
            br_info!("wifi: the chip had no station interface: created one");
            true
        }
        Err(e) => {
            br_error!("wifi: the Wi-Fi HAL: IWifiChip.createStaIface: {}", why(e));
            false
        }
    }
}

/// The session's return: the chip stopped again when this session started it, as One UI's switch had it. When One UI
/// had started it (the usual boot), it is left exactly as it was.
pub fn give_back() {
    let chip = {
        let mut slot = lock();
        match slot.take() {
            Some(chip) => chip,
            None => return,
        }
    };
    if chip.started {
        match chip.conn.call_void(chip.service, WIFI_STOP, &parcel_empty(WIFI_IWIFI)) {
            Ok(()) => br_info!("wifi: the chip was started for the session (One UI's Wi-Fi was off): stopped again"),
            Err(e) => br_error!("wifi: IWifi.stop: {} (the chip stays started; One UI's switch turns it off)", why(e)),
        }
    }
    chip.conn.release(chip.service);
}

/// `wifictl chip stop`: the chip stopped through its HAL, whoever started it (for a session that was killed: the
/// normal return does this itself). The state after it is One UI's Wi-Fi off, its driver unloaded.
pub fn stop(wait_ms: u32) -> Result<(), String> {
    let (conn, service) = attach(wait_ms)?;
    version(&conn, service)?;
    let started = is_started(&conn, service)?;
    if started {
        conn.call_void(service, WIFI_STOP, &parcel_empty(WIFI_IWIFI)).map_err(|e| format!("IWifi.stop: {}", why(e)))?;
    }
    conn.release(service);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_the_frozen_files() {
        // The place of each method in the frozen version 1 .aidl files. A drift here is a call to the wrong method.
        assert_eq!(
            [WIFI_GET_CHIP, WIFI_IS_STARTED, WIFI_START, WIFI_STOP],
            [1, 3, 5, 6],
            "IWifi: getChip, getChipIds, isStarted, registerEventCallback, start, stop"
        );
        assert_eq!(
            [CHIP_CREATE_STA_IFACE, CHIP_GET_STA_IFACE_NAMES],
            [7, 24],
            "IWifiChip: ... createStaIface is 7th; getStaIfaceNames the 24th"
        );
    }

    #[test]
    fn parcels_byte_for_byte() {
        // A token (strict-mode word, work source, 'SYST', length, UTF-16, NUL, padding), then the argument.
        let p = parcel_empty(WIFI_IWIFI);
        let mut want = vec![0, 0, 0, 0x80, 0xff, 0xff, 0xff, 0xff, 0x54, 0x53, 0x59, 0x53];
        want.extend_from_slice(&(WIFI_IWIFI.len() as u32).to_le_bytes());
        for b in WIFI_IWIFI.bytes() {
            want.extend_from_slice(&[b, 0]);
        }
        want.extend_from_slice(&[0, 0]); // the NUL, then the padding to 4 bytes
        while want.len() % 4 != 0 {
            want.push(0);
        }
        assert_eq!(p.bytes(), &want[..]);
        let p = parcel_i32(WIFI_IWIFI, 0);
        assert_eq!(&p.bytes()[want.len()..], &[0, 0, 0, 0]);
        // getService: the manager's token, then the name.
        let p = parcel_string(WIFI_MANAGER, WIFI_SERVICE);
        assert!(p.bytes().starts_with(&[0, 0, 0, 0x80, 0xff, 0xff, 0xff, 0xff, 0x54, 0x53, 0x59, 0x53]));
        assert!(p
            .bytes()
            .windows(WIFI_SERVICE.len() * 2)
            .any(|w| w == WIFI_SERVICE.bytes().flat_map(|b| [b, 0]).collect::<Vec<u8>>()));
    }

    #[test]
    fn the_station_names_are_a_string_array() {
        let mut p = Parcel::new();
        p.put_i32(2);
        p.put_str("wlan0");
        p.put_str("wlan1");
        let mut r = Reader::new(p.bytes(), &[]);
        assert_eq!(sta_names(&mut r), Ok(vec!["wlan0".to_owned(), "wlan1".to_owned()]));
        let mut p = Parcel::new();
        p.put_i32(-1);
        let mut r = Reader::new(p.bytes(), &[]);
        assert_eq!(sta_names(&mut r), Ok(Vec::new()));
        let mut p = Parcel::new();
        p.put_i32(17);
        let mut r = Reader::new(p.bytes(), &[]);
        assert_eq!(sta_names(&mut r), Err(-libc::EPROTO));
    }
}
