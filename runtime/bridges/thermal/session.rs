// SPDX-License-Identifier: MIT
//! The thermal bridge's session (thermal.c's second half): the worker that reads the sensors from the start (the cap
//! is the host's own protection and does not wait for the guest), its two listeners, the load line.

use super::{deci_text, decide, from_hal, level_name, parse_limits, Clocks, HostLoad, Limits, Raw, Sensors, State};
use super::{BY_BATTERY, BY_SKIN, BY_SOC, LOAD_MS, POLL_MS, SOC_SAMPLES_MAX, UNKNOWN};
use crate::hal::{self, Temperature};
use crate::hw::{self, Backend, Bridge};
use crate::{br_error, br_info};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use xh_core::thread::lock;
use xh_core::time::monotonic_ms;

/// A listener; runs on the worker thread without the bridge's locks.
pub type Listener = Arc<dyn Fn(&State) + Send + Sync>;

/// How the bridge opens. [`Options::from_env`] is what xnuhost opens with (C: `br_thermal_open(NULL)`).
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// None: the phone's paths
    pub root: Option<String>,
    /// 0: POLL_MS
    pub poll_ms: u32,
    /// None: the defaults (and, with `environment`, ONEIOS_THERMAL_SKIN, _SOC, _BATTERY, _SOC_SAMPLES)
    pub limits: Option<Limits>,
    /// The SoC rule raised to the kernel's trips (ONEIOS_THERMAL_SOC_TRIPS=1 in from_env)
    pub soc_trips: bool,
    /// 0: LOAD_MS, the "thermal: N s: host CPU" line's period
    pub load_ms: u32,
    /// Samsung, Treble; Unset: the process's hw-backend
    pub backend: Backend,
    /// Treble: the binder device; None: ONEIOS_BINDER, else /dev/binder
    pub binder: Option<String>,
    /// The environment's limits and ONEIOS_THERMAL_CAP are read (C: options NULL)
    pub environment: bool,
    /// The critical cell step is off whatever the environment says (xnuhost's `thermal-cap=0`, the default): the
    /// level still goes to iOS, the CPU keeps every core. See [`cap_for`].
    pub cap_off: bool,
}

impl Options {
    /// The phone and the environment.
    pub fn from_env() -> Options {
        Options {
            soc_trips: std::env::var("ONEIOS_THERMAL_SOC_TRIPS").is_ok_and(|v| v == "1"),
            environment: true,
            ..Options::default()
        }
    }
}

fn limits_from_env(l: &mut Limits) {
    fn take<const N: usize>(name: &str, into: &mut [i32; N]) {
        let Ok(text) = std::env::var(name) else { return };
        match parse_limits::<N>(&text) {
            Some(v) => *into = v,
            None => br_error!(
                "thermal: {name}=\"{text}\" is not {N} ascending degrees (\"39,42,44,46,50\"): the defaults stay"
            ),
        }
    }
    take("ONEIOS_THERMAL_SKIN", &mut l.skin);
    take("ONEIOS_THERMAL_SOC", &mut l.soc);
    take("ONEIOS_THERMAL_BATTERY", &mut l.battery);
    if let Ok(n) = std::env::var("ONEIOS_THERMAL_SOC_SAMPLES") {
        if !n.is_empty() {
            match n.parse::<usize>() {
                Ok(v) if (1..=SOC_SAMPLES_MAX).contains(&v) && !n.starts_with('+') => l.soc_samples = v,
                _ => {
                    br_error!("thermal: ONEIOS_THERMAL_SOC_SAMPLES=\"{n}\" is not 1 to {SOC_SAMPLES_MAX}: one reading")
                }
            }
        }
    }
}

/// Where the readings come from.
enum Source {
    Files,
    Hal(hal::Hal),
}

/// The treble backend's reading: one getTemperatures call (code 3 of the frozen IThermal.aidl).
fn hal_read(h: &hal::Hal) -> Result<(Raw, usize), i32> {
    let temps = h.call(hal::THERMAL_GET_TEMPERATURES, &hal::parcel(hal::THERMAL), |r| {
        Temperature::decode_all(r, hal::TEMPS_MAX)
    })?;
    Ok((from_hal(&temps), temps.len()))
}

struct Run {
    quit: bool,
    cap_fn: Option<Listener>,
    report_fn: Option<Listener>,
    now: State,
    sent: State,
    sent_ms: u64,
    cap_told: u32,
    /// The last load sample.
    load: HostLoad,
    /// The last clocks sample (the "thermal: N s clocks:" line, with the load line).
    clocks: Clocks,
    hal_failed: bool,
    /// Where the case came from in the treble backend: "SKIN" from the HAL, or "lrp"/"ap thermistor"/"unknown" when
    /// the HAL left SKIN out and the files answered (the open line says which).
    case_from: &'static str,
}

struct Shared {
    sensors: Sensors,
    source: Source,
    limits: Limits,
    poll: Duration,
    cap_off: bool,
    load_ms: u32,
    run: Mutex<Run>,
    wake: Condvar,
}

fn say_level(s: &State, was: &State) {
    let held = if s.soc_n > 1 {
        // ONEIOS_THERMAL_SOC_SAMPLES > 1: what the SoC rule read
        format!("; {} the lowest of {}", deci_text(s.soc_held), s.soc_n)
    } else {
        String::new()
    };
    let by = |bit: u32, text: &'static str| if s.reason & bit != 0 { text } else { "" };
    br_info!(
        "thermal: {} (was {}; by{}{}{}): case {}, SoC {} ({}{held}), battery {}",
        level_name(s.level),
        level_name(was.level),
        by(BY_SKIN, " case"),
        by(BY_SOC, " SoC"),
        by(BY_BATTERY, " battery"),
        deci_text(s.raw.skin),
        deci_text(s.raw.soc),
        if s.raw.soc_zone.is_empty() { "-" } else { &s.raw.soc_zone },
        deci_text(s.raw.battery)
    );
}

impl Shared {
    /// The reading of either backend. A HAL that stops answering: one line, the readings unknown until it answers
    /// again.
    fn sensors_read(&self, run: &mut Run) -> Raw {
        let Source::Hal(h) = &self.source else { return self.sensors.read() };
        match hal_read(h) {
            Ok((mut raw, _)) => {
                run.hal_failed = false;
                // The HAL does not always report every type: Samsung's may leave SKIN out. Keep the case and battery
                // rules by reading the files for what the HAL left unknown (in this mode the sensors hold no zones,
                // so this reads the board thermistor, the battery and lrp). The open line says which answered.
                let files = self.sensors.read();
                run.case_from = if raw.skin != UNKNOWN {
                    "SKIN"
                } else if files.skin == UNKNOWN {
                    "unknown"
                } else if files.lrp != UNKNOWN {
                    "lrp"
                } else {
                    "ap thermistor"
                };
                if raw.skin == UNKNOWN {
                    raw.skin = files.skin;
                }
                if raw.battery == UNKNOWN {
                    raw.battery = files.battery;
                }
                raw
            }
            Err(rc) => {
                if !run.hal_failed {
                    br_error!(
                        "thermal: the thermal HAL's getTemperatures failed ({}): the readings are unknown until it \
                         answers",
                        hal::error_text(rc)
                    );
                }
                run.hal_failed = true;
                run.case_from = "unknown";
                from_hal(&[])
            }
        }
    }

    /// With the lock held.
    fn step(&self, run: &mut Run) {
        let raw = self.sensors_read(run);
        let mut next = decide(&self.limits, &raw, Some(&run.now));
        if self.cap_off {
            next.cap = 0;
        }
        if next.level != run.now.level {
            say_level(&next, &run.now);
        }
        run.now = next;
    }

    /// With the lock held: every load_ms, the "thermal: N s: host CPU ..." line.
    fn load_tick(&self, run: &mut Run) {
        let now = HostLoad::read(&self.sensors.root);
        if now.ms.saturating_sub(run.load.ms) < u64::from(self.load_ms) {
            return;
        }
        let page = xh_core::sys::page_size();
        br_info!("thermal: {}", HostLoad::line(&run.load, &now, Some(&run.now.raw), page));
        run.load = now;
        let clocks = Clocks::read(&self.sensors.root);
        if let Some(line) = Clocks::line(&run.clocks, &clocks) {
            br_info!("thermal: {line}");
        }
        run.clocks = clocks;
    }

    fn worker(&self) {
        let mut run = lock(&self.run);
        while !run.quit {
            run = self.wake.wait_timeout(run, self.poll).unwrap_or_else(PoisonError::into_inner).0;
            if run.quit {
                break;
            }
            self.step(&mut run);
            self.load_tick(&mut run);
            let state = run.now.clone();
            let cap_fn = if run.cap_told != state.cap { run.cap_fn.clone() } else { None };
            if cap_fn.is_some() {
                run.cap_told = state.cap;
            }
            let now = monotonic_ms();
            let since = u32::try_from(now.saturating_sub(run.sent_ms)).unwrap_or(u32::MAX);
            let mut report_fn = None;
            if run.report_fn.is_some() && run.sent.changed(&state, since) {
                report_fn = run.report_fn.clone();
                run.sent = state.clone();
                run.sent_ms = now;
            }
            if cap_fn.is_none() && report_fn.is_none() {
                continue;
            }
            drop(run);
            if let Some(f) = cap_fn {
                f(&state);
            }
            if let Some(f) = report_fn {
                f(&state); // takes the bridge's lock
            }
            run = lock(&self.run);
        }
    }
}

/// An open bridge; dropping it (or [`Thermal::close`]) stops the worker.
pub struct Thermal {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Thermal {
    /// None (with a log line) when there is nothing to read. The worker runs from the start.
    pub fn open(o: &Options) -> Option<Thermal> {
        let root = o.root.clone().unwrap_or_default();
        let mut found = None;
        if hw::select(Bridge::Thermal, o.backend) == Backend::Treble {
            let why = match hal::Hal::open(o.binder.as_deref(), hal::THERMAL_INSTANCE, hal::THERMAL) {
                Err(why) => why,
                Ok(h) => match hal_read(&h) {
                    Ok((_, n)) if n > 0 => {
                        found = Some((h, n));
                        String::new()
                    }
                    Ok(_) => format!("{}: getTemperatures: no temperature", hal::THERMAL_INSTANCE),
                    Err(rc) => format!("{}: getTemperatures: {}", hal::THERMAL_INSTANCE, hal::error_text(rc)),
                },
            };
            if found.is_none() && !hw::missing(Bridge::Thermal, &why) {
                return None;
            }
        }
        let (sensors, source, hal_temps) = match found {
            Some((h, n)) => {
                let sensors =
                    Sensors { root, zones: Vec::new(), trip_first: UNKNOWN, trip_hot: UNKNOWN, trip_zones: 0 };
                (sensors, Source::Hal(h), n)
            }
            None => {
                let Some(sensors) = Sensors::find(&root) else {
                    br_info!("thermal: off: no thermal zone, board thermistor or battery temperature can be read");
                    return None;
                };
                (sensors, Source::Files, 0)
            }
        };
        // The limits: the caller's; else the defaults, with ONEIOS_THERMAL_SOC_TRIPS=1 (or options.soc_trips) the SoC's
        // raised to the phone's own trips (the files backend: the HAL has no trips here), then the environment's,
        // which win.
        let mut from_trips = false;
        let limits = match o.limits {
            Some(l) => l,
            None => {
                let mut l = Limits::default();
                if matches!(source, Source::Files) && o.soc_trips {
                    from_trips = l.from_trips(sensors.trip_first, sensors.trip_hot);
                }
                if o.environment {
                    limits_from_env(&mut l);
                }
                l
            }
        };
        let cap_off = o.cap_off || (o.environment && std::env::var("ONEIOS_THERMAL_CAP").is_ok_and(|v| v == "0"));
        let poll_ms = if o.poll_ms != 0 { o.poll_ms } else { POLL_MS };
        let load_ms = if o.load_ms != 0 { o.load_ms } else { LOAD_MS };
        let load = HostLoad::read(&sensors.root);
        let clocks = Clocks::read(&sensors.root);
        let shared = Shared {
            sensors,
            source,
            limits,
            poll: Duration::from_millis(poll_ms.into()),
            cap_off,
            load_ms,
            run: Mutex::new(Run { load, clocks, ..empty_run() }),
            wake: Condvar::new(),
        };
        let raw = {
            let mut run = lock(&shared.run);
            let raw = shared.sensors_read(&mut run);
            run.now = decide(&shared.limits, &raw, None);
            if shared.cap_off {
                run.now.cap = 0;
            }
            run.sent = run.now.clone();
            run.sent_ms = monotonic_ms();
            raw
        };
        let shared = Arc::new(shared);
        let worker = Arc::clone(&shared);
        let Ok(thread) = thread::Builder::new().name("thermal".into()).spawn(move || worker.worker()) else {
            br_error!("thermal: off: no thread");
            return None;
        };
        let t = Thermal { shared, thread: Some(thread) };
        let s = &t.shared;
        let (level, case_from) = {
            let run = lock(&s.run);
            (level_name(run.now.level), run.case_from)
        };
        let zone = if raw.soc_zone.is_empty() { "-" } else { &raw.soc_zone };
        let cap_note =
            if cap_off { "; thermal-cap=0 (ONEIOS_THERMAL_CAP=0): xnuhost does not hold itself back" } else { "" };
        let (skin, soc, battery) = (deci_text(raw.skin), deci_text(raw.soc), deci_text(raw.battery));
        match &s.source {
            Source::Hal(h) => br_info!(
                "thermal: on: the thermal HAL {} (version {}), {hal_temps} temperatures; case {skin} ({case_from}), SoC \
                 {soc} ({zone}), battery {battery}; {level}; read every {poll_ms} ms{cap_note}",
                hal::THERMAL_INSTANCE,
                h.version()
            ),
            Source::Files => br_info!(
                "thermal: on: {} zones; case {skin} ({}), SoC {soc} ({zone}), battery {battery}; {level}; read every \
                 {poll_ms} ms{cap_note}",
                s.sensors.zones.len(),
                if raw.lrp != UNKNOWN { "lrp" } else { "ap thermistor" }
            ),
        }
        // The SoC rule and the kernel's trips, on every phone (the files backend reads the trips once, in find): a
        // phone whose first passive trip sits at or under TRAPPING shows it here (docs/hardware.md 7h).
        let samples = s.limits.soc_samples.max(1);
        br_info!(
            "thermal: the SoC rule: heavy from {}, trapping from {}, on {}{samples} reading{}{}; the kernel's trips: \
             first passive {}, hot {} (the median of {} SoC zones)",
            deci_text(s.limits.soc[0]),
            deci_text(s.limits.soc[1]),
            if samples > 1 { "the lowest of the last " } else { "" },
            if samples > 1 { "s" } else { "" },
            if from_trips { ", raised to the kernel's trips (ONEIOS_THERMAL_SOC_TRIPS=1)" } else { "" },
            deci_text(s.sensors.trip_first),
            deci_text(s.sensors.trip_hot),
            s.sensors.trip_zones
        );
        Some(t)
    }

    /// Treble when the thermal HAL is read, Samsung for the files.
    pub fn backend(&self) -> Backend {
        match self.shared.source {
            Source::Hal(_) => Backend::Treble,
            Source::Files => Backend::Samsung,
        }
    }

    /// The last state (read at open, then by the worker).
    pub fn get(&self) -> State {
        lock(&self.shared.run).now.clone()
    }

    /// `cap` (xnuhost): runs on each new cap. None stops that listener. A new listener hears the current cap at the
    /// next read when it is not 0.
    pub fn on_cap(&self, listener: Option<Listener>) {
        let mut run = lock(&self.shared.run);
        run.cap_fn = listener;
        run.cap_told = 0;
        self.shared.wake.notify_all();
    }

    /// `report` (the bridge device): runs when State::changed says so. None stops it. The caller sends the current
    /// state itself (get).
    pub fn on_report(&self, listener: Option<Listener>) {
        let mut run = lock(&self.shared.run);
        run.report_fn = listener;
        run.sent = run.now.clone();
        run.sent_ms = monotonic_ms();
    }

    /// Stops the worker (the same as dropping it).
    pub fn close(self) {}
}

fn empty_run() -> Run {
    Run {
        quit: false,
        cap_fn: None,
        report_fn: None,
        now: State::default(),
        sent: State::default(),
        sent_ms: 0,
        cap_told: 0,
        load: HostLoad::default(),
        clocks: Clocks::default(),
        hal_failed: false,
        case_from: "unknown",
    }
}

impl Drop for Thermal {
    fn drop(&mut self) {
        lock(&self.shared.run).quit = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
