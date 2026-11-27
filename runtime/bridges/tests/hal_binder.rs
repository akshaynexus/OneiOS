// SPDX-License-Identifier: MIT
//! The treble backend's HAL bridges against fake HALs on a real binder driver (C: tests/test_hal_binder.c, the
//! C Makefile's `test-binder`): a binderfs in a user namespace (hal::binderfs::enter; skipped, with the reason printed,
//! without one). A child (the fake-hal program, hal::fake) plays servicemanager and the HALs, values from files of a
//! folder, read at every call; this process opens the bridges with hw-backend treble and checks what they read and
//! call: the lookup by name, the descriptor check, the parcels, a HAL that is not registered (the one missing line,
//! then off, or the samsung form with hw-backend-fallback=1), and a HAL whose process dies under the bridge.
//! harness = false: one process, one thread of tests (the process's hw settings and log sink are shared).

use onebridges::battery::{self, Battery};
use onebridges::hal::binderfs;
use onebridges::haptics::{self, Haptic, Impact};
use onebridges::hw::{hw_configure, Backend, HwSettings};
use onebridges::log::Capture;
use onebridges::thermal::{self, Thermal};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::sleep;
use std::time::Duration;

static CHECKS: AtomicU32 = AtomicU32::new(0);
static FAILURES: AtomicU32 = AtomicU32::new(0);

/// C's CHECK: counts, prints a failure with its line, goes on.
macro_rules! check {
    ($cond:expr) => {{
        let ok: bool = $cond;
        CHECKS.fetch_add(1, Ordering::Relaxed);
        if !ok {
            FAILURES.fetch_add(1, Ordering::Relaxed);
            println!("FAIL {}:{}: {}", file!(), line!(), stringify!($cond));
        }
    }};
}

struct Values(PathBuf);

impl Values {
    fn put(&self, name: &str, text: &str) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn remove(&self, name: &str) {
        let _ = fs::remove_file(self.0.join(name));
    }
    /// What the fake vibrator HAL was asked since the last look.
    fn vib_log(&self) -> String {
        let text = fs::read_to_string(self.0.join("vibrator.log")).unwrap_or_default();
        self.remove("vibrator.log");
        text
    }
}

/// The fake HALs, until killed.
struct Fake(Child);

impl Fake {
    fn start(values: &Path) -> Fake {
        let mut child =
            Command::new(env!("CARGO_BIN_EXE_fake-hal")).arg(values).stdout(Stdio::piped()).spawn().unwrap();
        let mut line = String::new();
        let _ = BufReader::new(child.stdout.take().unwrap()).read_line(&mut line);
        check!(line == "ready\n");
        Fake(child)
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn ms(n: u64) {
    sleep(Duration::from_millis(n));
}

fn fallback(on: bool) {
    let mut hw = HwSettings::default();
    if on {
        check!(hw.setting("hw-backend-fallback", "1").is_ok());
    }
    hw_configure(&hw);
}

fn test_battery(v: &Values, log: &Capture) {
    v.put(
        "health",
        "level=73\nstatus=2\npresent=1\ntemp=312\nvoltage_mv=4105\ncurrent_ua=1250000\ncycles=41\nfull_uah=4850000\n\
         design_uah=5000000\nusb=1\n",
    );
    let mut fake = Some(Fake::start(&v.0));
    let mut o = battery::Options {
        backend: Backend::Treble,
        dir: Some("/nonexistent".into()),
        poll_ms: 50,
        ..battery::Options::default()
    };
    log.clear();
    let b = Battery::open(&o);
    check!(b.is_some());
    check!(log.has("hw: battery: backend treble (the caller's choice)"));
    check!(log.has(
        "battery: on: the health HAL android.hardware.health.IHealth/default (version 2), status \"Charging\", 73%, \
         plugged in"
    ));
    if let Some(b) = b {
        let s = b.query();
        check!(s.is_ok());
        let s = s.unwrap_or_else(|s| s);
        check!(
            s.percent == 73
                && s.present
                && s.plugged
                && s.charging
                && !s.full
                && s.temp_deci_c == 312
                && s.voltage_mv == 4105
        );
        check!(s.current_ma == 1250 && s.full_mah == 4850 && s.design_mah == 5000 && s.cycles == 41);
        check!(s.minutes == (4850 - 4850 * 73 / 100) * 60 / 1250);
        // The HAL is asked again at every read: a change shows at the next query.
        v.put("health", "level=72\nstatus=3\npresent=1\ntemp=300\nvoltage_mv=4000\ncurrent_ua=-400000\ncycles=41\nfull_uah=4850000\n");
        let s = b.query();
        check!(s.is_ok_and(|s| s.percent == 72
            && !s.plugged
            && !s.charging
            && s.current_ma == -400
            && s.design_mah == 4850));
        // The HAL's process dies: the read fails, iOS keeps the last level, one error line.
        // The log is cleared before the HAL stops: the worker (50 ms) can read the dead HAL while the fake is stopped, and
        // it says so once only (hal_failed), so a clear after it could wipe the one line (a full parallel host-tests.sh
        // run of the C on the build laptop, 2026-11-23: the line printed, the check failed).
        log.clear();
        fake = None;
        check!(b.query().is_err_and(|last| last.percent == 72));
        check!(log.has("battery: the health HAL's getHealthInfo failed"));
        b.close();
    }
    drop(fake);

    // Not registered: the one missing line, then off; with hw-backend-fallback=1 the samsung nodes.
    v.remove("health");
    let _fake = Fake::start(&v.0);
    log.clear();
    check!(Battery::open(&o).is_none());
    check!(log.has(
        "hw: battery: treble: missing: android.hardware.health.IHealth/default (no service: not registered with \
         servicemanager, or SELinux denies the lookup); the bridge stays off (hw-backend-fallback=0)"
    ));
    v.put("power_supply/battery/capacity", "55\n");
    v.put("power_supply/battery/status", "Discharging\n");
    fallback(true);
    o.dir = Some(v.0.join("power_supply").display().to_string());
    log.clear();
    let f = Battery::open(&o);
    check!(
        f.is_some()
            && log.has("falling back to samsung (hw-backend-fallback=1)")
            && log.has("battery: on: ")
            && log.has("55%")
    );
    drop(f);
    fallback(false);
}

fn test_thermal(v: &Values, log: &Capture) {
    v.put("thermal", "0 cpu-0-0 48.5\n0 cpu-1-2 61.2\n1 gpu 55.0\n2 battery 30.4\n3 skin 37.1\n");
    let mut fake = Some(Fake::start(&v.0));
    let o = thermal::Options {
        backend: Backend::Treble,
        root: Some("/nonexistent".into()),
        poll_ms: 50,
        load_ms: 3_600_000,
        ..thermal::Options::default()
    };
    log.clear();
    let t = Thermal::open(&o);
    check!(t.is_some());
    check!(log.has("hw: thermal: backend treble (the caller's choice)"));
    check!(log.has(
        "thermal: on: the thermal HAL android.hardware.thermal.IThermal/default (version 2), 5 temperatures; case \
         37.1 C (SKIN), SoC 61.2 C (cpu-1-2), battery 30.4 C; nominal"
    ));
    if let Some(t) = t {
        let s = t.get();
        check!(t.backend() == Backend::Treble && s.raw.cpu == 612 && s.raw.gpu == 550 && s.raw.skin == 371);
        check!(s.raw.battery == 304 && s.level == thermal::NOMINAL);
        // The worker asks again: a hot case is the same level as Samsung's lrp would give.
        v.put("thermal", "0 cpu-1-2 70.0\n3 skin 44.5\n2 battery 33.0\n");
        ms(300);
        let s = t.get();
        check!(s.raw.skin == 445 && s.raw.cpu == 700 && s.level == thermal::HEAVY);
        // The log is cleared before the HAL stops: the worker (50 ms) can read the dead HAL while the fake is stopped,
        // and it says so once only (hal_failed), so a clear after it could wipe the one line (a full parallel
        // host-tests.sh run of the C on the build laptop, 2026-11-23: the line printed, the check failed).
        log.clear();
        fake = None;
        for _ in 0..60 {
            // the next reading, within 3 s
            if log.has("getTemperatures failed") {
                break;
            }
            ms(50);
        }
        ms(100);
        let s = t.get();
        check!(log.has("thermal: the thermal HAL's getTemperatures failed") && s.raw.skin == thermal::UNKNOWN);
        t.close();
    }
    drop(fake);
    // A HAL that leaves SKIN out (Samsung's may): the case comes from the files, and the open line says which
    // answered (2026-11-25: the S25's lrp read about 10 C under the back glass).
    v.put("thermal", "0 cpu-0-0 48.5\n1 gpu 55.0\n2 battery 30.4\n");
    v.put("files-root/sys/class/power_supply/battery/lrp", "470\n");
    v.put("files-root/sys/class/power_supply/battery/temp", "455\n");
    let fake2 = Fake::start(&v.0);
    let o2 = thermal::Options {
        backend: Backend::Treble,
        root: Some(v.0.join("files-root").display().to_string()),
        poll_ms: 50,
        load_ms: 3_600_000,
        ..thermal::Options::default()
    };
    log.clear();
    let t = Thermal::open(&o2);
    check!(t.is_some());
    check!(log.has("(version 2), 3 temperatures; case 47.0 C (lrp)"));
    if let Some(t) = t {
        check!(t.get().raw.skin == 470);
        t.close();
    }
    drop(fake2);
    v.remove("thermal");
    let _fake = Fake::start(&v.0);
    log.clear();
    check!(Thermal::open(&o).is_none());
    check!(log.has("hw: thermal: treble: missing: android.hardware.thermal.IThermal/default (no service: "));
    check!(log.has("the bridge stays off"));
    v.put("thermal", ""); // registered, but no temperature
    log.clear();
    check!(Thermal::open(&o).is_none());
    check!(log.has("treble: missing: android.hardware.thermal.IThermal/default: getTemperatures: no temperature"));
}

fn test_haptics(v: &Values, log: &Capture) {
    v.put("vibrator", "4\n"); // CAP_AMPLITUDE_CONTROL
    let _fake = Fake::start(&v.0);
    let open = || Haptic::open_as(Some("/nonexistent"), Backend::Treble, None);
    log.clear();
    let h = open();
    check!(h.is_some() && log.has("hw: haptics: backend treble (the caller's choice)"));
    check!(log.has(
        "haptics: on: the vibrator HAL android.hardware.vibrator.IVibrator/default (version 2, capabilities 0x4: \
         amplitude control"
    ));
    if let Some(h) = h {
        h.vib(30, 1000, 0); // full intensity: amplitude 1
        h.flush();
        check!(v.vib_log() == "on 30\namplitude 1.00\n");
        h.vib(80, 500, 0); // half intensity: level 147 of 255
        h.flush();
        let text = v.vib_log();
        check!(text.contains("on 80\namplitude 0.58\n") && !text.contains("refused"));
        h.stop();
        h.flush();
        check!(v.vib_log() == "off\n");
        let st = h.stats();
        check!(st.played == 2 && st.stopped >= 1 && st.failed == 0);
        drop(h); // the close stops the motor
        check!(v.vib_log() == "off\n");
    }
    // No amplitude control: on() only.
    v.put("vibrator", "0\n");
    let h = open();
    check!(h.is_some() && log.has("no amplitude control"));
    if let Some(h) = h {
        h.vib(20, 300, 0);
        h.flush();
        check!(v.vib_log() == "on 20\n");
        drop(h);
        v.vib_log();
    }
    // Explicit native impact mode, never used by default. The HAL advertises all three standard presets in the fake
    // HAL, and perform() is serialized through the same worker as ordinary VIB and STOP. (C: ONEIOS_HAPTIC_NATIVE_
    // IMPACTS=1; here the process's setting, which wins over the variable, so no setenv beside the bridges' threads.)
    haptics::set_native_impacts(Some(true));
    v.put("vibrator", "36\n"); // amplitude + compose
    let h = open();
    check!(h.as_ref().is_some_and(Haptic::impacts_available));
    if let Some(h) = h {
        check!(h.impact(Impact::Heavy as u32).is_ok());
        h.flush();
        check!(v.vib_log() == "perform 5 2\n");
        let st = h.stats();
        check!(st.impacts == 1 && st.played == 1 && st.failed == 0);
        ms(120); // fake HAL says 60ms; it finished naturally
        h.stop();
        h.flush();
        check!(v.vib_log().is_empty()); // no late off() to interrupt One UI
        drop(h);
        check!(v.vib_log().is_empty()); // no unconditional off() after an impact
    }
    v.put("vibrator_effects_no_heavy_click", "1\n");
    let h = open();
    check!(h.as_ref().is_some_and(|h| !h.impacts_available()));
    if let Some(h) = h {
        check!(h.impact(Impact::Heavy as u32) == Err(libc::EOPNOTSUPP));
        drop(h);
        check!(!v.vib_log().contains("perform"));
    }
    v.remove("vibrator_effects_no_heavy_click");
    haptics::set_native_impacts(None);
    // Not registered: the missing line, off; with the fallback the evdev search (nothing under /nonexistent here).
    v.remove("vibrator");
    log.clear();
    check!(open().is_none());
    check!(log.has("hw: haptics: treble: missing: android.hardware.vibrator.IVibrator/default (no service: "));
    check!(log.has("the bridge stays off") && !log.has("no force-feedback device"));
    fallback(true);
    log.clear();
    check!(open().is_none() && log.has("falling back to samsung"));
    check!(log.has("haptics: off: no force-feedback device with FF_PERIODIC + FF_SINE under /nonexistent"));
    fallback(false);
}

fn main() {
    let device = match binderfs::enter("ONEIOS_BINDER", "binder") {
        binderfs::Entered::Device(path) => path,
        binderfs::Entered::Skipped(why) => {
            println!("hal_binder: skipped ({why})");
            return;
        }
        binderfs::Entered::Ran(code) => std::process::exit(code),
    };
    println!("hal_binder: on {device}");
    let values = Values(std::env::temp_dir().join(format!("oneios-hal-{}", std::process::id())));
    let _ = fs::remove_dir_all(&values.0);
    fs::create_dir_all(&values.0).unwrap();
    let log = Capture::start();
    test_battery(&values, &log);
    test_thermal(&values, &log);
    test_haptics(&values, &log);
    let (checks, failures) = (CHECKS.load(Ordering::Relaxed), FAILURES.load(Ordering::Relaxed));
    if failures != 0 {
        for (level, line) in log.lines() {
            println!("  [{}] {line}", if level == onebridges::log::Level::Error { "E" } else { "I" });
        }
    }
    drop(log);
    let _ = fs::remove_dir_all(&values.0);
    println!("hal binder: {checks} checks, {failures} failed");
    std::process::exit(i32::from(failures != 0));
}
