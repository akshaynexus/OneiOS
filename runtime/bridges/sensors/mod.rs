// SPDX-License-Identifier: MIT
//! The sensors bridge: the Galaxy S24 Ultra's sensor hub (Qualcomm SSC / SEE on the ADSP) read directly over Linux's
//! QRTR, with no Android framework (system_server is gone in a session): the accelerometer, the gyroscope, the
//! magnetometer and the FRONT ambient light sensor (the rear one, `als_rear` on evdev, gives 0 lux on the S24).
//! README.md here says why this route and what is not known yet.
//!
//! The SEE client is ported from DenialOfGravity/ios-redfin 23019ab4 `phone/sensors/` (MIT, Copyright (c) 2026
//! dipa-studio): `sns.c`/`pb.c` (message build and parse: sns.rs, pb.rs) and `sensor.c` (the lookup, attribute and
//! stream sequence, the data types: here). Their QRTR device for XNU is not needed: Linux has AF_QIPCRTR (qrtr.rs).
//!
//! Each sensor is one QMI client (its own QRTR socket), as in redfin's sensd and the Android HAL. [`Hub::open`] finds
//! the SNS service (400) and each sensor's SUID (and the light sensors' attributes, to take the front one); nothing is
//! streamed yet. [`Hub::start`] streams them on one thread that blocks in poll() on the sockets and an eventfd: no
//! timer, no polling loop. Streams stop while iOS's display is off ([`display`], from the bridge device's power
//! message) and when the hub is closed; a killed process ends them too (its sockets close: the kernel tells the hub).
//!
//! Rate: the accelerometer and the gyroscope at `hz` (default 50), the magnetometer at min(hz, 25), the light on
//! change. One motion frame to the guest per accelerometer sample. So about 125 wake-ups a second of this thread and 50
//! frames a second to the guest while iOS's display is on; with it off one wake-up a minute (the stats line).

pub mod pb;
pub mod qrtr;
pub mod sns;
#[cfg(test)]
mod tests;

use crate::{br_error, br_info};
use qrtr::{Server, Socket};
use sns::{Attrs, Sns, Suid};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use xh_core::thread::lock;

/// The sensors this bridge reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Accel,
    Gyro,
    Mag,
    Light,
}

pub const KINDS: [Kind; 4] = [Kind::Accel, Kind::Gyro, Kind::Mag, Kind::Light];

impl Kind {
    /// The SEE data type (redfin sensor.c's table).
    pub fn data_type(self) -> &'static str {
        match self {
            Kind::Accel => "accel",
            Kind::Gyro => "gyro",
            Kind::Mag => "mag",
            Kind::Light => "ambient_light",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Kind::Accel => "accelerometer",
            Kind::Gyro => "gyroscope",
            Kind::Mag => "magnetometer",
            Kind::Light => "light",
        }
    }
    fn index(self) -> usize {
        self as usize
    }
}

/// How the hub opens.
#[derive(Clone, Debug)]
pub struct Options {
    /// The accelerometer's and the gyroscope's rate (the magnetometer's is at most 25 Hz)
    pub hz: u32,
    /// Which ambient_light SUID (the lookup's order); None: the first whose name and vendor do not say "rear"
    pub light_index: Option<usize>,
    pub motion: bool,
    pub light: bool,
    /// Each request's answer is waited for at most this long
    pub timeout: Duration,
}

impl Default for Options {
    fn default() -> Options {
        Options { hz: 50, light_index: None, motion: true, light: true, timeout: Duration::from_millis(1500) }
    }
}

/// The motion sensors' latest values, in the phone's (Android's) frame and units: m/s^2 (the reaction to gravity: face
/// up on a table z = +9.8), rad/s, microtesla.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Motion {
    pub accel: [f32; 3],
    pub gyro: [f32; 3],
    pub mag: [f32; 3],
    pub have_gyro: bool,
    pub have_mag: bool,
}

/// The guest's motion device units (UniXNU-droidvm's virtio motion device, DroidVMMotionHid.inc): the gravity vector
/// 256 per g in iOS's sign (face up z = -256), angular velocity 16 per degree per second, the magnetic field 16 per
/// microtesla.
pub const PER_G: f32 = 256.0;
pub const PER_DEG_S: f32 = 16.0;
pub const PER_UT: f32 = 16.0;
/// The axes' ranges as the device states them: 16 g, 2000 degrees per second, 2000 microtesla.
pub const ACCEL_MAX: i32 = 16 * 256;
pub const GYRO_MAX: i32 = 2000 * 16;
pub const MAG_MAX: i32 = 2000 * 16;
const STANDARD_GRAVITY: f32 = 9.80665;

fn clamp(v: f32, max: i32) -> i32 {
    if v.is_nan() {
        return 0;
    }
    (v.round() as i64).clamp(-i64::from(max), i64::from(max)) as i32
}

/// The motion frame for the guest: accel x y z, gyro x y z, mag x y z, in the guest device's units. Android's
/// accelerometer reads the reaction to gravity; iOS's (and the device's) the gravity vector: the sign is turned round
/// (Jesvi-iOS inject_sensors.c, redfin 0971's accsign). The gyroscope (right-handed, counter-clockwise positive) and
/// the magnetometer keep their signs. The phone's frame is iOS's panel frame: both are portrait, y up the panel.
pub fn axes(m: &Motion) -> [i32; 9] {
    let a = |v: f32| clamp(-v / STANDARD_GRAVITY * PER_G, ACCEL_MAX);
    let g = |v: f32| clamp(v.to_degrees() * PER_DEG_S, GYRO_MAX);
    let b = |v: f32| clamp(v * PER_UT, MAG_MAX);
    [
        a(m.accel[0]),
        a(m.accel[1]),
        a(m.accel[2]),
        g(m.gyro[0]),
        g(m.gyro[1]),
        g(m.gyro[2]),
        b(m.mag[0]),
        b(m.mag[1]),
        b(m.mag[2]),
    ]
}

/// Lux to millilux, saturating.
pub fn millilux(lux: f32) -> u32 {
    if lux.is_nan() || lux <= 0.0 {
        return 0;
    }
    let v = (f64::from(lux) * 1000.0).round();
    if v >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        v as u32
    }
}

/// Which light sensor to stream: `index` when it names one, else the first whose name and vendor do not say "rear",
/// else the first.
pub fn pick_light(found: &[(Suid, Option<Attrs>)], index: Option<usize>) -> Option<usize> {
    if found.is_empty() {
        return None;
    }
    if let Some(i) = index.filter(|&i| i < found.len()) {
        return Some(i);
    }
    let rear = |a: &Option<Attrs>| {
        a.as_ref().is_some_and(|a| format!("{} {}", a.name, a.vendor).to_ascii_lowercase().contains("rear"))
    };
    Some(found.iter().position(|(_, a)| !rear(a)).unwrap_or(0))
}

/// "name" vendor, streaming at 5..500 Hz: for the logs.
pub fn describe(a: &Attrs) -> String {
    let mut s = format!("\"{}\" ({})", a.name, a.vendor);
    match a.stream_type {
        Some(sns::STREAM_ON_CHANGE) => s += ", on change",
        Some(sns::STREAM_STREAMING) => s += ", streaming",
        _ => {}
    }
    if let (Some(lo), Some(hi)) = (a.rates.first(), a.rates.last()) {
        s += &format!(" {lo}..{hi} Hz");
    }
    s
}

/// The motion listener: one call per accelerometer sample, on the hub's thread; false when the frame was dropped (the
/// guest had no room).
pub type MotionFn = Arc<dyn Fn(&Motion) -> bool + Send + Sync>;
/// The light listener: one call per reading, millilux, on the hub's thread.
pub type LightFn = Arc<dyn Fn(u32) + Send + Sync>;

/// One sensor: its client socket and SUID.
struct Sensor {
    kind: Kind,
    socket: Socket,
    suid: Suid,
    attrs: Option<Attrs>,
    on_change: bool,
    txn: u16,
    active: bool,
    /// its socket failed: no more streaming from it
    dead: bool,
}

/// One request's answer, and what came with it, on a socket of the open's: (response's result, SUID events, attribute
/// events by SUID).
#[derive(Default)]
struct Answer {
    result: Option<(u16, u16)>,
    suids: Vec<(String, Vec<Suid>)>,
    attrs: Vec<(Suid, Attrs)>,
}

impl Sensor {
    fn request(&mut self, service: &Server, suid: &Suid, msg_id: u32, payload: &[u8]) -> io::Result<u16> {
        let req = sns::client_request(suid, msg_id, payload, sns::DELIVERY_WAKEUP);
        let txn = self.txn;
        self.txn = self.txn.wrapping_add(1).max(1);
        let q = sns::qmi_request(txn, &req, true)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "request too long"))?;
        self.socket.send(service.node, service.port, &q)?;
        Ok(txn)
    }

    /// A request and its answer: waits for the response and then until `done` says what came is enough.
    fn ask(
        &mut self,
        service: &Server,
        suid: &Suid,
        msg_id: u32,
        payload: &[u8],
        timeout: Duration,
        done: impl Fn(&Answer) -> bool,
    ) -> io::Result<Answer> {
        let txn = self.request(service, suid, msg_id, payload)?;
        let deadline = Instant::now() + timeout;
        let mut buf = vec![0u8; qrtr::MAX_PACKET];
        let mut a = Answer::default();
        loop {
            if a.result.is_some_and(|(r, _)| r != 0) || (a.result.is_some() && done(&a)) {
                return Ok(a);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no answer"));
            }
            let Some(p) = self.socket.recv(&mut buf, Some(left))? else { continue };
            if p.port == qrtr::PORT_CTRL {
                continue;
            }
            match sns::parse(&buf[..p.len]) {
                Some(Sns::Response { txn: t, result, error, .. }) if t == txn => a.result = Some((result, error)),
                Some(Sns::Report { payload, .. }) => {
                    let Some((from, events)) = sns::events(payload) else { continue };
                    for e in events {
                        if from == sns::SUID_LOOKUP && e.msg_id == sns::MSG_SUID_EVENT {
                            a.suids.extend(sns::suid_event(e.payload));
                        } else if e.msg_id == sns::MSG_ATTR_EVENT {
                            a.attrs.extend(sns::attr_event(e.payload).map(|x| (from, x)));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn start(&mut self, service: &Server, hz: f32) -> io::Result<()> {
        let suid = self.suid;
        if self.on_change {
            self.request(service, &suid, sns::MSG_ONCHANGE_CONFIG, &[])?;
        } else {
            self.request(service, &suid, sns::MSG_SENSOR_CONFIG, &sns::sensor_config(hz))?;
        }
        self.active = true;
        Ok(())
    }

    fn stop(&mut self, service: &Server) {
        if self.active && !self.dead {
            let suid = self.suid;
            let _ = self.request(service, &suid, sns::MSG_DISABLE, &[]);
        }
        self.active = false;
    }
}

/// The counters of the stats line.
#[derive(Default)]
struct Counts {
    samples: [AtomicU64; 4],
    frames: AtomicU64,
    dropped: AtomicU64,
    last_millilux: AtomicU64,
    refused: AtomicU64,
}

/// What the open found, for the log and for xnuhost.
#[derive(Clone, Debug, Default)]
pub struct Found {
    pub service: Server,
    /// Each kind found: its SUID and a description
    pub sensors: Vec<(Kind, Suid, String)>,
    /// The light sensors there were, and which one is used
    pub lights: usize,
}

/// An open sensor hub.
pub struct Hub {
    found: Found,
    sensors: Mutex<Vec<Sensor>>,
    hz: u32,
    wake: OwnedFd,
    active: AtomicBool,
    stop: AtomicBool,
    counts: Counts,
    thread: Mutex<Option<JoinHandle<()>>>,
}

/// The data types `sensorctl qrtr --sensors` asks the hub for (redfin sensor.c's, and the S24's other hub sensors).
pub const LIST_TYPES: [&str; 8] = ["accel", "gyro", "mag", "ambient_light", "proximity", "pressure", "gravity", "rotv"];

/// What the hub has of each data type: every SUID with its attributes, or why not. Read only: lookups and attribute
/// requests, no stream (`sensorctl qrtr --sensors`). Err: the SNS service is not there.
#[allow(clippy::type_complexity)]
pub fn list(
    types: &[&str],
    timeout: Duration,
) -> Result<(Server, Vec<(String, Result<Vec<(Suid, Option<Attrs>)>, String>)>), String> {
    let control = Socket::open().map_err(|e| format!("socket(AF_QIPCRTR): {}", xh_core::sys::error_text(&e)))?;
    let servers = control
        .lookup(sns::SERVICE, timeout)
        .map_err(|e| format!("the QRTR name server: {}", xh_core::sys::error_text(&e)))?;
    let service = *servers.first().ok_or("the QRTR name server has no service 400")?;
    let mut out = Vec::new();
    for dt in types {
        let one = || -> Result<Vec<(Suid, Option<Attrs>)>, String> {
            let socket = Socket::open().map_err(|e| xh_core::sys::error_text(&e))?;
            let mut s = Sensor {
                kind: Kind::Accel,
                socket,
                suid: Suid::default(),
                attrs: None,
                on_change: false,
                txn: 1,
                active: false,
                dead: false,
            };
            let a = s
                .ask(&service, &sns::SUID_LOOKUP, sns::MSG_SUID_REQ, &sns::suid_req(dt, false, false), timeout, |a| {
                    a.suids.iter().any(|(t, _)| t == dt)
                })
                .map_err(|e| format!("no answer to the lookup: {}", xh_core::sys::error_text(&e)))?;
            if let Some((r, e)) = a.result.filter(|(r, _)| *r != 0) {
                return Err(format!("the lookup was refused (result {r}, error {e})"));
            }
            let suids = a.suids.into_iter().find(|(t, _)| t == dt).map(|(_, v)| v).unwrap_or_default();
            let mut found = Vec::new();
            for suid in suids {
                let attrs = s
                    .ask(&service, &suid, sns::MSG_ATTR_REQ, &sns::attr_req(false), timeout, |a| {
                        a.attrs.iter().any(|(f, _)| *f == suid)
                    })
                    .ok()
                    .and_then(|a| a.attrs.into_iter().find(|(f, _)| *f == suid).map(|(_, x)| x));
                found.push((suid, attrs));
            }
            Ok(found)
        };
        out.push((dt.to_string(), one()));
    }
    Ok((service, out))
}

/// The running hub, for [`display`].
static ACTIVE: Mutex<Option<Weak<Hub>>> = Mutex::new(None);

/// iOS's display went on or off (the bridge device's power message): streams stop while it is off.
pub fn display(on: bool) {
    let hub = lock(&ACTIVE).as_ref().and_then(Weak::upgrade);
    if let Some(h) = hub {
        h.set_active(on);
    }
}

fn eventfd() -> io::Result<OwnedFd> {
    // SAFETY: a plain eventfd; owned below.
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a new descriptor of ours.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

impl Hub {
    /// Finds the SNS service and the sensors; streams nothing. Err: why the hub cannot be used (no QRTR, no service
    /// 400, no sensor answered).
    pub fn open(o: &Options) -> Result<Hub, String> {
        let control = Socket::open().map_err(|e| format!("socket(AF_QIPCRTR): {}", xh_core::sys::error_text(&e)))?;
        let servers = control
            .lookup(sns::SERVICE, o.timeout)
            .map_err(|e| format!("the QRTR name server: {}", xh_core::sys::error_text(&e)))?;
        drop(control);
        let Some(service) = servers.first().copied() else {
            return Err("the QRTR name server has no service 400 (the sensor hub's SNS client service)".into());
        };
        let mut found = Found { service, ..Found::default() };
        let mut sensors = Vec::new();
        let kinds: Vec<Kind> =
            KINDS.iter().copied().filter(|k| if *k == Kind::Light { o.light } else { o.motion }).collect();
        for kind in kinds {
            let socket = match Socket::open() {
                Ok(s) => s,
                Err(e) => {
                    br_error!("sensors: {}: socket: {}", kind.name(), xh_core::sys::error_text(&e));
                    continue;
                }
            };
            let mut s = Sensor {
                kind,
                socket,
                suid: Suid::default(),
                attrs: None,
                on_change: false,
                txn: 1,
                active: false,
                dead: false,
            };
            // the light sensors all (to take the front one), the others the default one
            let lookup = sns::suid_req(kind.data_type(), false, kind != Kind::Light);
            let dt = kind.data_type();
            let answer = s.ask(&service, &sns::SUID_LOOKUP, sns::MSG_SUID_REQ, &lookup, o.timeout, |a| {
                a.suids.iter().any(|(t, _)| t == dt)
            });
            let suids = match answer {
                Ok(a) if a.result.is_some_and(|(r, _)| r != 0) => {
                    let (r, e) = a.result.unwrap();
                    br_info!("sensors: {}: the hub refused the lookup (result {r}, error {e})", kind.name());
                    continue;
                }
                Ok(a) => a.suids.into_iter().find(|(t, _)| t == dt).map(|(_, v)| v).unwrap_or_default(),
                Err(e) => {
                    br_info!(
                        "sensors: {}: no answer to the lookup of \"{dt}\": {}",
                        kind.name(),
                        xh_core::sys::error_text(&e)
                    );
                    continue;
                }
            };
            if suids.is_empty() {
                br_info!("sensors: {}: the hub has no \"{dt}\" sensor", kind.name());
                continue;
            }
            // the attributes: the name for the log, the stream type, and which light is the front one
            let mut cands: Vec<(Suid, Option<Attrs>)> = Vec::new();
            for suid in suids.iter().take(if kind == Kind::Light { 4 } else { 1 }) {
                let attrs = s
                    .ask(&service, suid, sns::MSG_ATTR_REQ, &sns::attr_req(false), o.timeout, |a| {
                        a.attrs.iter().any(|(f, _)| f == suid)
                    })
                    .ok()
                    .and_then(|a| a.attrs.into_iter().find(|(f, _)| f == suid).map(|(_, x)| x));
                cands.push((*suid, attrs));
            }
            let Some(pick) = (if kind == Kind::Light { pick_light(&cands, o.light_index) } else { Some(0) }) else {
                continue;
            };
            if kind == Kind::Light {
                found.lights = cands.len();
                for (i, (suid, a)) in cands.iter().enumerate() {
                    br_info!(
                        "sensors: light {i}: {suid} {}{}",
                        a.as_ref().map_or_else(|| "(no attributes)".to_string(), describe),
                        if i == pick { ": used" } else { "" }
                    );
                }
            }
            let (suid, attrs) = cands.swap_remove(pick);
            s.suid = suid;
            s.on_change =
                kind == Kind::Light || attrs.as_ref().is_some_and(|a| a.stream_type == Some(sns::STREAM_ON_CHANGE));
            found.sensors.push((kind, suid, attrs.as_ref().map_or_else(|| "(no attributes)".to_string(), describe)));
            s.attrs = attrs;
            sensors.push(s);
        }
        if sensors.is_empty() {
            return Err(format!(
                "service 400 at node {} port {} answered no lookup of accel, gyro, mag or ambient_light",
                service.node, service.port
            ));
        }
        let wake = eventfd().map_err(|e| format!("eventfd: {}", xh_core::sys::error_text(&e)))?;
        Ok(Hub {
            found,
            sensors: Mutex::new(sensors),
            hz: o.hz.clamp(5, 200),
            wake,
            active: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            counts: Counts::default(),
            thread: Mutex::new(None),
        })
    }

    pub fn found(&self) -> &Found {
        &self.found
    }

    pub fn has(&self, kind: Kind) -> bool {
        self.found.sensors.iter().any(|(k, ..)| *k == kind)
    }

    pub fn hz(&self) -> u32 {
        self.hz
    }

    /// Streams on a thread of its own: `motion` once per accelerometer sample, `light` once per reading.
    pub fn start(self: &Arc<Hub>, motion: Option<MotionFn>, light: Option<LightFn>) -> bool {
        let me = Arc::clone(self);
        let Ok(handle) = xh_core::thread::spawn("sensors", move || me.worker(motion, light)) else {
            br_error!("sensors: the hub's thread did not start: nothing is streamed");
            return false;
        };
        *lock(&self.thread) = Some(handle);
        *lock(&ACTIVE) = Some(Arc::downgrade(self));
        true
    }

    fn poke(&self) {
        let one = 1u64.to_ne_bytes();
        // SAFETY: 8 bytes to our eventfd.
        let _ = unsafe { libc::write(self.wake.as_raw_fd(), one.as_ptr().cast(), 8) };
    }

    /// Streams on (iOS's display on) or off.
    pub fn set_active(&self, on: bool) {
        if self.active.swap(on, Relaxed) != on {
            self.poke();
        }
    }

    /// Every stream stopped, the sockets closed, the thread ended. Within a few milliseconds.
    pub fn close(&self) {
        self.stop.store(true, Relaxed);
        self.poke();
        if let Some(h) = lock(&self.thread).take() {
            let _ = h.join();
        }
        let service = self.found.service;
        for s in lock(&self.sensors).iter_mut() {
            s.stop(&service);
        }
        *lock(&ACTIVE) = None;
    }

    fn rate(&self, kind: Kind) -> f32 {
        if kind == Kind::Mag {
            self.hz.min(25) as f32
        } else {
            self.hz as f32
        }
    }

    fn set_streams(&self, sensors: &mut [Sensor], on: bool) {
        let service = self.found.service;
        for s in sensors.iter_mut().filter(|s| !s.dead) {
            if on && !s.active {
                if let Err(e) = s.start(&service, self.rate(s.kind)) {
                    br_error!(
                        "sensors: {}: the stream could not be asked for: {}",
                        s.kind.name(),
                        xh_core::sys::error_text(&e)
                    );
                    s.dead = true;
                }
            } else if !on {
                s.stop(&service);
            }
        }
    }

    fn worker(&self, motion: Option<MotionFn>, light: Option<LightFn>) {
        let mut sensors = std::mem::take(&mut *lock(&self.sensors));
        let mut on = self.active.load(Relaxed);
        self.set_streams(&mut sensors, on);
        let mut m = Motion::default();
        let mut buf = vec![0u8; qrtr::MAX_PACKET];
        let period = Duration::from_secs(60);
        let mut next_stats = Instant::now() + period;
        let (mut paused_since, mut paused_ms) = (if on { None } else { Some(Instant::now()) }, 0u128);
        let mut shown = [0u64; 7];
        while !self.stop.load(Relaxed) {
            // the eventfd, then each live sensor (live: their indices, in the same order)
            let live: Vec<usize> = (0..sensors.len()).filter(|&i| !sensors[i].dead).collect();
            let mut fds: Vec<libc::pollfd> =
                vec![libc::pollfd { fd: self.wake.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
            fds.extend(live.iter().map(|&i| libc::pollfd {
                fd: sensors[i].socket.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            }));
            let left = next_stats.saturating_duration_since(Instant::now()).as_millis().min(60_000) as i32;
            // SAFETY: fds is ours for the call.
            let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, left) };
            if n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                br_error!(
                    "sensors: poll: {}: the hub's thread ends",
                    xh_core::sys::error_text(&io::Error::last_os_error())
                );
                break;
            }
            if fds[0].revents != 0 {
                let mut v = [0u8; 8];
                // SAFETY: 8 bytes from our eventfd.
                let _ = unsafe { libc::read(self.wake.as_raw_fd(), v.as_mut_ptr().cast(), 8) };
                let want = self.active.load(Relaxed);
                if want != on && !self.stop.load(Relaxed) {
                    on = want;
                    self.set_streams(&mut sensors, on);
                    if on {
                        paused_ms += paused_since.take().map_or(0, |t| t.elapsed().as_millis());
                        br_info!("sensors: resumed: iOS's display is on, every stream asked for again");
                        // the last reading at once: auto-brightness's first sample after a wake
                        if let (Some(f), last) = (&light, self.counts.last_millilux.load(Relaxed)) {
                            if self.counts.samples[Kind::Light.index()].load(Relaxed) != 0 {
                                f(last as u32);
                            }
                        }
                    } else {
                        paused_since = Some(Instant::now());
                        br_info!("sensors: paused: iOS's display is off, every stream stopped on the hub");
                    }
                }
            }
            let ready: Vec<usize> = live
                .iter()
                .zip(&fds[1..])
                .filter(|(_, p)| p.revents != 0)
                .map(|(&i, _)| i)
                .filter(|&i| !sensors[i].dead)
                .collect();
            for i in ready {
                // everything that is there, then back to poll
                loop {
                    let s = &mut sensors[i];
                    let p = match s.socket.recv_now(&mut buf) {
                        Ok(p) => p,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => {
                            br_error!(
                                "sensors: {}: the hub's socket failed ({}): this sensor streams no more",
                                s.kind.name(),
                                xh_core::sys::error_text(&e)
                            );
                            s.dead = true;
                            break;
                        }
                    };
                    if p.port == qrtr::PORT_CTRL {
                        continue;
                    }
                    let kind = s.kind;
                    self.take(kind, &buf[..p.len], &mut m, motion.as_ref(), light.as_ref());
                }
            }
            if Instant::now() >= next_stats {
                next_stats += period;
                if next_stats < Instant::now() {
                    next_stats = Instant::now() + period;
                }
                let paused = paused_ms + paused_since.map_or(0, |t| t.elapsed().as_millis());
                paused_ms = 0;
                if paused_since.is_some() {
                    paused_since = Some(Instant::now());
                }
                self.stats_line(&mut shown, paused);
            }
        }
        let service = self.found.service;
        for s in sensors.iter_mut() {
            s.stop(&service);
        }
        *lock(&self.sensors) = sensors;
    }

    /// One datagram of a sensor's socket.
    fn take(&self, kind: Kind, d: &[u8], m: &mut Motion, motion: Option<&MotionFn>, light: Option<&LightFn>) {
        match sns::parse(d) {
            Some(Sns::Response { result, error, .. }) if result != 0 => {
                if self.counts.refused.fetch_add(1, Relaxed) < 8 {
                    br_info!("sensors: {}: the hub refused a request (result {result}, error {error})", kind.name());
                }
            }
            Some(Sns::Report { payload, .. }) => {
                let Some((_, events)) = sns::events(payload) else { return };
                for e in events {
                    if e.msg_id == sns::MSG_ERROR_EVENT {
                        br_info!("sensors: {}: the hub's error event {:?}", kind.name(), sns::error_event(e.payload));
                        continue;
                    }
                    if e.msg_id < 1024 {
                        continue; // attributes, flushes, the physical config
                    }
                    let Some((v, _status)) = sns::sensor_event(e.payload) else { continue };
                    if v.is_empty() {
                        continue;
                    }
                    self.counts.samples[kind.index()].fetch_add(1, Relaxed);
                    let three = |v: &[f32]| [v[0], *v.get(1).unwrap_or(&0.0), *v.get(2).unwrap_or(&0.0)];
                    match kind {
                        Kind::Accel => {
                            m.accel = three(&v);
                            if let Some(f) = motion {
                                if f(m) {
                                    self.counts.frames.fetch_add(1, Relaxed);
                                } else {
                                    self.counts.dropped.fetch_add(1, Relaxed);
                                }
                            }
                        }
                        Kind::Gyro => {
                            m.gyro = three(&v);
                            m.have_gyro = true;
                        }
                        Kind::Mag => {
                            m.mag = three(&v);
                            m.have_mag = true;
                        }
                        Kind::Light => {
                            let mlx = millilux(v[0]);
                            self.counts.last_millilux.store(u64::from(mlx), Relaxed);
                            if let Some(f) = light {
                                f(mlx);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// The 60 s line: what came in the last minute (nothing when nothing came and nothing changed).
    fn stats_line(&self, shown: &mut [u64; 7], paused_ms: u128) {
        let c = &self.counts;
        let now = [
            c.samples[0].load(Relaxed),
            c.samples[1].load(Relaxed),
            c.samples[2].load(Relaxed),
            c.samples[3].load(Relaxed),
            c.frames.load(Relaxed),
            c.dropped.load(Relaxed),
            c.last_millilux.load(Relaxed),
        ];
        let d: Vec<u64> = now.iter().zip(shown.iter()).map(|(a, b)| a.wrapping_sub(*b)).collect();
        *shown = now;
        if d[..6].iter().all(|&x| x == 0) && paused_ms >= 59_000 {
            return; // a whole minute paused: no line
        }
        let lux = now[6];
        br_info!(
            "sensors: 60 s: accelerometer {}, gyroscope {}, magnetometer {} samples; {} motion frames to the guest, {} \
             dropped (the guest had no buffers); light {} readings, last {}.{:03} lux; paused {} s",
            d[0],
            d[1],
            d[2],
            d[4],
            d[5],
            d[3],
            lux / 1000,
            lux % 1000,
            paused_ms / 1000
        );
    }
}

impl Drop for Hub {
    /// Asks the thread to stop (it may be the thread dropping the last reference: no join here; [`Hub::close`] joins).
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        self.poke();
    }
}
