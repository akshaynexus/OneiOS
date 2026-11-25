// SPDX-License-Identifier: MIT
//! Host unit tests of the brightness bridge (test_brightness.c, test_inspect.sh's cases are in brightctl.rs): the
//! curve, the fade, the light sensor's events and its conversion, and the session on a fake backlight device (a
//! folder). No phone.

use super::*;
use crate::hw::{Backend, HwSettings};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

// The kernel's codes, written out so the test does not depend on the module's constants.
const T_SYN: u16 = 0;
const T_REL: u16 = 2;
const T_ABS: u16 = 3;
const C_REL_X: u16 = 0;
const C_REL_RX: u16 = 3;
const C_REL_RY: u16 = 4;
const C_REL_RZ: u16 = 5;
const C_ABS_Y: u16 = 1;
const C_ABS_Z: u16 = 2;

fn ms(n: u64) {
    sleep(Duration::from_millis(n));
}

#[test]
fn curve() {
    // Samsung's doubled ranges end the slider at the manual maximum; any other range is used whole.
    assert!(ceiling(510) == 255 && ceiling(51000) == 25500);
    assert!(ceiling(255) == 255 && ceiling(25500) == 25500 && ceiling(4095) == 4095);

    // The ends: 0 is the floor (never 0), full is the ceiling.
    assert!(map(0, false, 1, 255) == 1 && map(FULL, false, 1, 255) == 255);
    assert!(map(0, true, 1, 25500) == 1 && map(FULL, true, 1, 25500) == 25500);
    assert_eq!(map(FULL + 99, false, 1, 255), 255); // clamped
                                                    // Half the slider is a quarter of the light; half of a linear level is half.
    assert_eq!(map(FULL / 2, false, 0, 25500), 6375);
    assert_eq!(map(FULL / 2, true, 0, 25500), 12750);
    assert!(matches!(map(FULL / 2, false, 1, 255), 64 | 65));
    // Never falling, and inside the range, over the whole scale and for both ranges.
    for linear in [false, true] {
        let mut last = 0;
        for level in (0..=FULL).step_by(5) {
            let v = map(level, linear, 1, 25500);
            assert!(v >= last && (1..=25500).contains(&v));
            last = v;
        }
    }
    // A range with nothing in it does not divide by zero.
    assert!(map(1000, false, 7, 7) == 7 && map(1000, true, 9, 7) == 7);

    // The fade: a line from one level to the other, both ways, and no fade is a jump.
    assert!(
        fade_at(0, 60000, 0, 300) == 0 && fade_at(0, 60000, 150, 300) == 30000 && fade_at(0, 60000, 300, 300) == 60000
    );
    assert!(fade_at(60000, 0, 100, 300) == 40000 && fade_at(60000, 0, 999, 300) == 0);
    assert_eq!(fade_at(123, 456, 0, 0), 456);
}

#[test]
fn als() {
    let mut f = AlsFrame::default();
    // One report as stk_sec_report sends it: infrared, UV, clear (each + 1), the gains, the sync.
    assert!(!f.feed(T_REL, C_REL_X, 41));
    assert!(!f.feed(T_REL, C_REL_RX, 11));
    assert!(!f.feed(T_REL, C_REL_RY, 351));
    assert!(!f.feed(T_ABS, C_ABS_Y, 17));
    assert!(!f.feed(T_ABS, C_ABS_Z, 17));
    assert!(f.feed(T_SYN, 0, 0));
    assert!(f.clear == 350 && f.ir == 40 && f.uv == 10 && f.clear_gain == 16 && f.ir_gain == 16);
    assert!(f.millilux(1000) == 350000 && f.millilux(250) == 87500);
    // A sync without a clear value (the flush event, or the flicker frequency alone) is not a reading.
    assert!(!f.feed(T_SYN, 0, 0));
    assert!(!f.feed(T_REL, C_REL_RZ, 100) && !f.feed(T_SYN, 0, 0));
    // Darkness is sent as 1 (the driver adds 1 so the event is not dropped): 0 counts, 0 lux.
    assert!(!f.feed(T_REL, C_REL_RY, 1) && f.feed(T_SYN, 0, 0) && f.clear == 0 && f.millilux(1000) == 0);
    // A value that could not come from the driver does not go negative; a huge one saturates.
    assert!(!f.feed(T_REL, C_REL_RY, -5) && f.feed(T_SYN, 0, 0) && f.clear == 0);
    f.clear = u32::MAX;
    assert_eq!(f.millilux(1000), u32::MAX);

    // What is sent on: not faster than 5 a second, a tenth of change (1 lux in the dark), a repeat at 2 s.
    assert!(!als_changed(100000, 500000, 50));
    assert!(als_changed(100000, 111000, 250) && !als_changed(100000, 109000, 250) && als_changed(100000, 89000, 250));
    assert!(als_changed(0, 1000, 250) && !als_changed(0, 900, 250) && !als_changed(2000, 2500, 250));
    assert!(als_changed(100000, 100000, 2000));
}

/// The C header the guest's driver reads: its fraction's full scale is this bridge's level scale; the HID report
/// carries a u32 of millilux, as AlsFrame::millilux gives it.
#[test]
fn guest_headers() {
    let drivers = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../drivers/brightness");
    let define = |file: &str, name: &str| -> u64 {
        let text = fs::read_to_string(drivers.join(file)).unwrap();
        let line = text.lines().find(|l| l.starts_with(&format!("#define {name} "))).unwrap();
        let value = line.split_whitespace().nth(2).unwrap().trim_end_matches('u');
        match value.strip_prefix("0x") {
            Some(hex) => u64::from_str_radix(hex, 16).unwrap(),
            None => value.parse().unwrap(),
        }
    };
    assert_eq!(define("oneios_bl_request.h", "OBL_FRACTION_FULL"), u64::from(FULL));
    assert_eq!(define("oneios_als_hid.h", "ONEIOS_ALS_REPORT_SIZE"), 1 + std::mem::size_of::<u32>() as u64);
}

// ---- fake sysfs ----------------------------------------------------------------------------------

/// A folder that is removed at the end.
pub(super) struct Tmp(pub PathBuf);

impl Tmp {
    pub fn new(what: &str) -> Tmp {
        let dir = std::env::temp_dir().join(format!(
            "oneios-{what}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Tmp(dir)
    }
    pub fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn put(dir: &Path, name: &str, text: &str) {
    fs::write(dir.join(name), text).unwrap();
}

fn get(dir: &Path, name: &str) -> u32 {
    fs::read_to_string(dir.join(name))
        .ok()
        .and_then(|t| t.split(|c: char| !c.is_ascii_digit()).next().and_then(|d| d.parse().ok()))
        .unwrap_or(u32::MAX)
}

fn mkdir(path: &Path) {
    fs::create_dir_all(path).unwrap();
}

/// The writes of a session, as "p:255 t:2,0 ...".
#[derive(Clone, Default)]
struct Trace(Arc<Mutex<String>>);

impl Trace {
    fn hook(&self) -> TraceFn {
        let into = Arc::clone(&self.0);
        Arc::new(move |what: &str, value: &str| {
            let mut t = into.lock().unwrap();
            if !t.is_empty() {
                t.push(' ');
            }
            t.push(what.as_bytes()[0] as char);
            t.push(':');
            t.push_str(value);
        })
    }
    /// Whether the writes so far are `want`; forgets them.
    fn is(&self, want: &str) -> bool {
        let mut t = self.0.lock().unwrap();
        let same = *t == want;
        if !same {
            println!("  trace: {t}\n  want:  {want}");
        }
        t.clear();
        same
    }
    fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

fn touch_node(sec: &Path, folder: &str, name: &str, enabled_is_folder: bool) {
    mkdir(&sec.join(folder).join("input"));
    put(&sec.join(folder).join("input"), "name", name);
    if enabled_is_folder {
        mkdir(&sec.join(folder).join("enabled"));
    } else {
        put(&sec.join(folder), "enabled", "0,0\n");
    }
}

// ---- the touch IC follows the display ------------------------------------------------------------

#[test]
fn touch() {
    let _hw = crate::hw::settings_lock();
    let tmp = Tmp::new("touch");
    let (panel, sec, nosec) = (tmp.path("panel"), tmp.path("sec"), tmp.path("nosec"));
    let tsp = sec.join("tsp");
    mkdir(&panel);
    mkdir(&sec);
    mkdir(&nosec);
    put(&panel, "max_brightness", "255\n");
    put(&panel, "brightness", "100\n");
    touch_node(&sec, "tsp", "sec_touchscreen\n", false);
    touch_node(&sec, "tsp_pen", "sec_e-pen\n", false); // a tsp* folder that is not the touchscreen
    touch_node(&sec, "sec_epen", "sec_touchscreen\n", false); // the right name under a folder that is not tsp*
    touch_node(&sec, "tsp_pad", "sec_touchpad\n", false);
    let trace = Trace::default();

    let mut o = Options {
        backlight_dir: Some(tmp.0.clone()),
        touch_dir: Some(sec.clone()),
        no_als: true,
        trace: Some(trace.hook()),
        ..Options::default()
    };
    let b = Bright::open(&o).unwrap();
    let i = b.info();
    // The start: the one touchscreen is woken, the panel (lit by One UI) is not written.
    assert!(i.touch_nodes == 1 && i.start == 100 && trace.is("t:2,0 t:2,1"));
    assert_eq!(get(&tsp, "enabled"), 2);
    // A level change with the display on touches the panel only.
    b.set(FULL, false, 0);
    ms(80);
    assert!(trace.is("p:255"));
    // A level of 0 with the display on (iOS's brightness-nits 0, phone 2026-11-13 +4805): the panel at its floor, the
    // IC left awake.
    b.set(0, false, 0);
    ms(80);
    assert!(trace.is("p:1") && get(&tsp, "enabled") == 2);
    b.set(0, false, 100); // nor a fade down to 0
    ms(200);
    b.set(FULL, false, 0);
    ms(80);
    assert!(trace.is("p:255"));
    // iOS's display off (its framebuffer's power request): the IC first (1,0 then 1,1), then the backlight; once.
    b.display(false, Some("test"));
    ms(80);
    assert!(trace.is("t:1,0 t:1,1 p:1") && get(&tsp, "enabled") == 1);
    b.display(false, Some("test"));
    ms(80);
    assert!(trace.is(""));
    // On again: the level from before the off, then 2,0 and 2,1; a second on wakes the IC again.
    b.display(true, Some("test"));
    ms(80);
    assert!(trace.is("p:255 t:2,0 t:2,1") && get(&tsp, "enabled") == 2);
    b.display(true, Some("test"));
    ms(80);
    assert!(trace.is("t:2,0 t:2,1"));
    // A power-key press wakes the IC whatever it is believed to be (the phone's 2026-11-13 key press found it off).
    b.display(false, Some("test"));
    ms(80);
    assert!(trace.is("t:1,0 t:1,1 p:1"));
    b.key();
    ms(80);
    assert!(trace.is("t:2,0 t:2,1") && get(&tsp, "enabled") == 2);
    b.key();
    ms(80);
    assert!(trace.is("t:2,0 t:2,1"));
    // A level iOS sets while its display is off is taken; the on that follows does not undo it.
    b.set(FULL / 2, true, 0);
    ms(80);
    assert!(trace.is("p:128"));
    b.display(true, Some("test"));
    ms(80);
    assert!(trace.is("t:2,0 t:2,1"));
    // The pen's and the touchpad's nodes were never written.
    assert!(
        get(&sec, "tsp_pen/enabled") == 0 && get(&sec, "sec_epen/enabled") == 0 && get(&sec, "tsp_pad/enabled") == 0
    );
    // Closed while the display is off: One UI's level, then the IC awake again (the return).
    b.display(false, Some("test"));
    ms(80);
    assert!(trace.is("t:1,0 t:1,1 p:1"));
    b.close();
    assert!(trace.is("p:100 t:2,0 t:2,1") && get(&tsp, "enabled") == 2 && get(&panel, "brightness") == 100);

    // ONEIOS_TSP_OFF=0: the display's off leaves the IC awake.
    o.no_touch_off = true;
    let b = Bright::open(&o).unwrap();
    trace.is("t:2,0 t:2,1");
    b.display(false, Some("test"));
    ms(80);
    assert!(trace.is("p:1"));
    b.close();
    trace.is("p:100 t:2,0 t:2,1");
    o.no_touch_off = false;
    // The node set outside the bridge during the session (`oneios-tool phone backlight set N`): the close writes One
    // UI's value back even when the bridge's own last write was that same value.
    let b = Bright::open(&o).unwrap();
    ms(80);
    trace.clear();
    put(&panel, "brightness", "5350\n");
    b.close();
    assert_eq!(get(&panel, "brightness"), 100);
    trace.clear();
    // One UI left the panel at 0: iOS starts from the middle of the slider (a quarter of the light), the panel is lit
    // before the IC is woken, and the close leaves it lit.
    put(&panel, "brightness", "0\n");
    let b = Bright::open(&o).unwrap();
    let i = b.info();
    assert!(i.saved == 0 && i.start == 64 && get(&panel, "brightness") == 64 && trace.is("p:64 t:2,0 t:2,1"));
    assert!(b.level() > 32000 && b.level() < 33500);
    b.set(0, false, 0);
    ms(80);
    assert!(trace.is("p:1"));
    b.close();
    assert!(trace.is("p:64 t:2,0 t:2,1") && get(&panel, "brightness") == 64);
    o.start_percent = 100;
    put(&panel, "brightness", "0\n");
    let b = Bright::open(&o).unwrap();
    assert_eq!(get(&panel, "brightness"), 255);
    b.close();
    o.start_percent = 0;
    put(&panel, "brightness", "100\n");
    trace.clear();
    // The S Pen's IC (ONEIOS_PEN_IC=1, xnuhost with the host's pen): sec_epen/enabled beside the touch IC, in the
    // same writes, but only when its input/ link is the pen's (here it first says sec_touchscreen: not taken).
    o.pen_ic = true;
    let b = Bright::open(&o).unwrap();
    let i = b.info();
    assert!(i.touch_nodes == 1 && i.pen_nodes == 0 && trace.is("t:2,0 t:2,1") && get(&sec, "sec_epen/enabled") == 0);
    b.close();
    trace.clear();
    let pen_input = sec.join("sec_epen/input");
    put(&pen_input, "name", "sec_e-pen\n");
    let b = Bright::open(&o).unwrap();
    let i = b.info();
    // Woken at the start with the touch IC; asleep with it at iOS's display off; woken at the on, at a power-key
    // press and at the close. The touchpad's node is never written.
    assert!(i.touch_nodes == 1 && i.pen_nodes == 1 && trace.is("t:2,0 t:2,1 t:2,0 t:2,1"));
    assert!(get(&sec, "sec_epen/enabled") == 2 && get(&tsp, "enabled") == 2);
    b.display(false, Some("test"));
    ms(80);
    assert!(get(&sec, "sec_epen/enabled") == 1 && get(&tsp, "enabled") == 1);
    b.key();
    ms(80);
    assert!(get(&sec, "sec_epen/enabled") == 2 && get(&tsp, "enabled") == 2);
    b.display(true, Some("test"));
    ms(80);
    b.display(false, Some("test"));
    ms(80);
    assert!(get(&sec, "sec_epen/enabled") == 1 && get(&tsp, "enabled") == 1);
    b.close();
    assert!(get(&sec, "sec_epen/enabled") == 2 && get(&tsp, "enabled") == 2 && get(&sec, "tsp_pad/enabled") == 0);
    trace.clear();
    // A pen IC without an input/ link: taken by its folder's name alone. Without pen_ic: never written.
    fs::remove_dir_all(&pen_input).unwrap();
    put(&sec, "sec_epen/enabled", "0,0\n");
    let b = Bright::open(&o).unwrap();
    assert!(b.info().pen_nodes == 1 && get(&sec, "sec_epen/enabled") == 2);
    b.close();
    trace.clear();
    o.pen_ic = false;
    put(&sec, "sec_epen/enabled", "0,0\n");
    let b = Bright::open(&o).unwrap();
    assert!(b.info().pen_nodes == 0 && get(&sec, "sec_epen/enabled") == 0);
    b.close();
    assert_eq!(get(&sec, "sec_epen/enabled"), 0);
    trace.clear();
    // A phone without the node: no touch writes, the panel works.
    o.touch_dir = Some(nosec.clone());
    let b = Bright::open(&o).unwrap();
    assert_eq!(b.info().touch_nodes, 0);
    b.set(0, false, 0);
    ms(80);
    b.set(FULL, false, 0);
    ms(80);
    b.close();
    assert!(trace.is("p:1 p:255 p:100"));
    // A write that fails (the file cannot be opened for writing): tried, logged, and the panel still follows.
    touch_node(&nosec, "tsp", "sec_touchscreen\n", true);
    let b = Bright::open(&o).unwrap();
    assert!(b.info().touch_nodes == 1 && trace.is("t:2,0 t:2,1"));
    b.set(FULL, false, 0);
    ms(80);
    b.display(false, Some("test"));
    ms(80);
    b.display(true, Some("test"));
    ms(80);
    assert!(trace.is("p:255 t:1,0 t:1,1 p:1 p:255 t:2,0 t:2,1") && get(&panel, "brightness") == 255);
    b.close();
}

#[test]
fn session() {
    let _hw = crate::hw::settings_lock();
    let tmp = Tmp::new("bright");
    let (other, panel, empty) = (tmp.path("aaa-led"), tmp.path("panel"), tmp.path("none"));
    mkdir(&other);
    mkdir(&panel);
    mkdir(&empty);
    put(&other, "max_brightness", "100\n");
    put(&other, "brightness", "50\n");
    put(&panel, "max_brightness", "51000\n");
    put(&panel, "brightness", "9000\n");

    let mut o = Options { backlight_dir: Some(tmp.0.clone()), no_als: true, ..Options::default() };
    let b = Bright::open(&o).unwrap();
    let i = b.info();
    // Samsung's "panel" wins over another device, the slider ends at the manual maximum.
    assert!(i.panel && !i.als && i.node.contains("/panel/brightness") && i.max_brightness == 51000);
    assert!(i.ceiling == 25500 && i.floor == 1 && i.saved == 9000);
    assert_eq!(b.current(), 9000);
    // Where One UI left the panel, on the slider's scale: the map of that level is the value again (to a step).
    let at = map(b.level(), false, i.floor, i.ceiling);
    assert!((8999..=9001).contains(&at));
    b.set(FULL, false, 0);
    ms(100);
    assert!(get(&panel, "brightness") == 25500 && b.current() == 25500 && b.level() == FULL);
    b.set(0, false, 0); // iOS's display off: the floor, not 0
    ms(100);
    assert_eq!(get(&panel, "brightness"), 1);
    b.set(FULL / 2, true, 0);
    ms(100);
    assert_eq!(get(&panel, "brightness"), 12750);
    // A fade passes through values between the two and ends on the target.
    b.set(FULL, true, 200);
    ms(100);
    let mid = get(&panel, "brightness");
    assert!(mid > 12750 && mid < 25500);
    ms(250);
    assert_eq!(get(&panel, "brightness"), 25500);
    // A fade cut short by the close still leaves One UI's value.
    b.set(0, false, 5000);
    ms(50);
    b.close();
    assert_eq!(get(&panel, "brightness"), 9000);
    assert_eq!(get(&other, "brightness"), 50); // the other device was never touched

    // The ceiling and the floor can be set; a ceiling past the device's range is not taken.
    o.ceiling = 30000;
    o.floor = 300;
    let b = Bright::open(&o).unwrap();
    assert!(b.info().ceiling == 30000 && b.info().floor == 300);
    b.close();
    o.ceiling = 99999;
    o.floor = 0;
    let b = Bright::open(&o).unwrap();
    assert_eq!(b.info().ceiling, 25500);
    b.close();
    // No device at all: no bridge.
    o.backlight_dir = Some(empty);
    o.ceiling = 0;
    assert!(Bright::open(&o).is_none());
}

/// The treble backend (hw-backend-backlight=treble): the standard class names only, the whole range, the touch IC
/// and the light sensor left alone; a phone without one of those names: one line, then off, or samsung with fallback.
#[test]
fn treble() {
    let hw_lock = crate::hw::settings_lock();
    let tmp = Tmp::new("treble");
    let (qcom, other, sec, only) = (tmp.path("panel0-backlight"), tmp.path("panel"), tmp.path("sec"), tmp.path("only"));
    let foo = only.join("vendor-bl");
    for d in [&qcom, &other, &sec, &only, &foo] {
        mkdir(d);
    }
    put(&qcom, "max_brightness", "4095\n");
    put(&qcom, "brightness", "2000\n");
    put(&other, "max_brightness", "51000\n");
    put(&other, "brightness", "9000\n");
    put(&foo, "max_brightness", "510\n");
    put(&foo, "brightness", "100\n");
    touch_node(&sec, "tsp", "sec_touchscreen", false);

    // samsung (the default) and treble on the same sysfs: samsung takes Samsung's "panel" (halved range) and wakes
    // the IC; treble takes the standard panel0-backlight first, its whole range, and never writes the IC's node.
    let mut o = Options {
        backlight_dir: Some(tmp.0.clone()),
        touch_dir: Some(sec.clone()),
        no_als: true,
        ..Options::default()
    };
    let b = Bright::open(&o).unwrap();
    let i = b.info();
    assert!(
        i.backend == Backend::Samsung
            && i.node.contains("/panel/brightness")
            && i.ceiling == 25500
            && i.touch_nodes == 1
    );
    b.close();
    assert_eq!(get(&other, "brightness"), 9000);
    put(&sec, "tsp/enabled", "0,0\n");
    o.backend = Backend::Treble;
    let b = Bright::open(&o).unwrap();
    let i = b.info();
    assert!(i.backend == Backend::Treble && i.panel && i.node.contains("/panel0-backlight/brightness"));
    assert!(i.max_brightness == 4095 && i.ceiling == 4095 && i.saved == 2000);
    assert!(i.touch_nodes == 0 && !i.als);
    b.set(FULL, true, 0);
    ms(100);
    assert_eq!(get(&qcom, "brightness"), 4095);
    b.display(false, Some("test")); // display off: the panel's floor, no IC write
    ms(100);
    assert_eq!(get(&qcom, "brightness"), 1);
    b.display(true, Some("test"));
    ms(100);
    assert_eq!(get(&qcom, "brightness"), 4095);
    b.key(); // nothing to wake
    b.close();
    assert!(get(&qcom, "brightness") == 2000 && get(&other, "brightness") == 9000); // One UI's value back; Samsung's node untouched
    assert_eq!(fs::read_to_string(sec.join("tsp/enabled")).unwrap(), "0,0\n"); // the treble session never wrote the IC's node

    // A phone whose backlight has another name: samsung lists the folder and finds it; treble does not list folders,
    // says so in one line and stays off; with hw-backend-fallback=1 it uses the samsung form.
    o.backlight_dir = Some(only.clone());
    o.backend = Backend::Samsung;
    let b = Bright::open(&o).unwrap();
    assert!(b.info().node.contains("/vendor-bl/brightness") && b.info().ceiling == 255);
    b.close();
    o.backend = Backend::Treble;
    assert!(Bright::open(&o).is_none());
    assert_eq!(get(&foo, "brightness"), 100);
    let mut s = HwSettings::default();
    assert_eq!(s.setting("hw-backend-fallback", "1"), Ok(()));
    hw_lock.configure(&s);
    let b = Bright::open(&o).unwrap();
    assert!(b.info().backend == Backend::Samsung && b.info().node.contains("/vendor-bl/brightness"));
    b.close();
    // The process's setting: hw-backend-backlight=treble with no forced choice.
    let mut s = HwSettings::default();
    assert_eq!(s.setting("hw-backend-backlight", "treble"), Ok(()));
    hw_lock.configure(&s);
    o.backlight_dir = Some(tmp.0.clone());
    o.backend = Backend::Unset;
    let b = Bright::open(&o).unwrap();
    assert!(b.info().backend == Backend::Treble && b.info().node.contains("/panel0-backlight/"));
    b.close();
}

/// Galaxy S26 Ultra manual slider cap, observed while Android 17 / One UI 9 was still in control: system
/// screen_brightness=255 maps to backlight=2300, despite panel0-backlight/max_brightness=12750. This is a tested
/// configuration value, NOT a claim about HBM or the true panel maximum. No active phone writes.
#[test]
fn sm8850_manual_brightness_cap() {
    let _hw = crate::hw::settings_lock();
    let tmp = Tmp::new("sm8850-bright");
    let (disabled, active) = (tmp.path("panel"), tmp.path("panel0-backlight"));
    mkdir(&disabled);
    mkdir(&active);
    put(&disabled, "max_brightness", "0\n");
    put(&disabled, "brightness", "0\n");
    put(&active, "max_brightness", "12750\n");
    put(&active, "brightness", "2300\n");
    let o = Options {
        backlight_dir: Some(tmp.0.clone()),
        touch_dir: Some("/nonexistent".into()),
        no_als: true,
        ceiling: 2300, // opt-in port profile; existing S24 default remains unchanged
        ..Options::default()
    };
    let b = Bright::open(&o).unwrap();
    let i = b.info();
    assert!(i.panel && i.node.contains("/panel0-backlight/brightness"));
    assert!(i.max_brightness == 12750 && i.ceiling == 2300 && i.saved == 2300);
    b.set(FULL, false, 0);
    ms(80);
    assert_eq!(get(&active, "brightness"), 2300);
    b.set(FULL / 2, false, 0);
    ms(80);
    assert!((574..=576).contains(&get(&active, "brightness")));
    b.set(FULL + 5000, true, 0); // clamp invalid guest slider
    ms(80);
    assert_eq!(get(&active, "brightness"), 2300);
    b.close();
    assert!(get(&active, "brightness") == 2300 && get(&disabled, "brightness") == 0);
}

/// The light from the sensor hub (sensors=ssc|auto): no evdev sensor is opened, each change of a tenth (or 1 lux) goes
/// to the listener at once, small ones are held back until 2 s have passed.
#[test]
fn external_light() {
    let _hw = crate::hw::settings_lock();
    let tmp = Tmp::new("bright-ext");
    let panel = tmp.path("panel");
    mkdir(&panel);
    put(&panel, "max_brightness", "255\n");
    put(&panel, "brightness", "100\n");
    let o = Options {
        backlight_dir: Some(tmp.0.clone()),
        // no input folder at all: the evdev lookup would find nothing, and must not be tried
        input_dir: Some(tmp.path("no-input")),
        als_external: Some("the sensor hub's ambient_light 0 (front)".into()),
        ..Options::default()
    };
    let b = Bright::open(&o).unwrap();
    let i = b.info();
    assert!(i.panel && i.als && i.als_node == "the sensor hub's ambient_light 0 (front)");
    b.als_feed(5_000); // no listener yet: nothing, and nothing kept
    let heard = Arc::new(Mutex::new(Vec::new()));
    let h2 = Arc::clone(&heard);
    b.listen(Some(Arc::new(move |s: &AlsSample| h2.lock().unwrap().push(s.millilux))));
    for v in [140_000, 145_000, 160_000, 160_500, 900, 1_500, 2_100] {
        b.als_feed(v);
    }
    // 145 lux is within a tenth of 140; 160.5 within a tenth of 160; 1.5 lux within 1 lux of 0.9; 2.1 is 1.2 lux off
    assert_eq!(*heard.lock().unwrap(), [140_000, 160_000, 900, 2_100]);
    b.listen(None);
    b.als_feed(500_000);
    assert_eq!(heard.lock().unwrap().len(), 4);
    b.close();
}
