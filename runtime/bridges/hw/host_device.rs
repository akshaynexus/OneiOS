// SPDX-License-Identifier: MIT
//! The host phone and its defaults (C: hw/host_device.c, include/bridges/host_device.h): which known phone xnuhost
//! (and each bridge's ctl tool) runs on, and that phone's defaults (docs/hardware.md, "The host device";
//! docs/architecture.md, "The host device table"). One tree, many phones: the table here is the one place that says
//! what differs per phone, and it holds only settings that already exist as options (an xnuhost setting KEY=VALUE, or
//! an environment variable a bridge reads).
//!
//! Detection reads, once at the start and read only:
//!   the properties ro.product.model, ro.product.device, ro.board.platform, ro.soc.model, ro.soc.manufacturer
//!     (bionic's property area; or a getprop dump given as a file: tests, a PC replaying a phone)
//!   and as fallbacks /sys/devices/soc0/machine, /sys/devices/soc0/soc_id (logged only) and /proc/device-tree/compatible
//!     (under a root folder that tests replace)
//! and matches, in this order: a model prefix (ro.product.model), a codename (ro.product.device), the SoC (ro.soc.model,
//! then soc0's machine), the platform (ro.board.platform), the device tree's compatible list. The first entry that
//! matches wins. No match: no entry (today's defaults) and a line that says the phone is unknown and what was read.
//!
//! Precedence (xnuhost's config; the ctl tools likewise): an explicit setting (the command line, the image set's
//! xnuhost.conf, an environment variable that is set) > the entry's default > the built-in default.
//! Settings: xnuhost `host-device=auto|none|NAME` (auto: detect; none: no entry, today's defaults; NAME: that entry,
//! whatever the phone says), `host-props=FILE`, `host-root=DIR`; the ctl tools ONEIOS_HOST_DEVICE, ONEIOS_HOST_PROPS,
//! ONEIOS_HOST_ROOT.

use super::hw_backend::HwSettings;
use crate::log::truncate;
use std::io::{BufRead, BufReader};

/// PROP_VALUE_MAX: a property's value is at most 91 bytes.
pub const HOST_VALUE: usize = 92;

/// One known phone.
#[derive(Debug, PartialEq, Eq)]
pub struct HostDevice {
    /// the host-device= name: "galaxy-s24-ultra"
    pub name: &'static str,
    /// "Galaxy S24 Ultra (SM-S928, SM8650)"
    pub label: &'static str,
    /// ro.product.model prefixes ("SM-S928": SM-S928B, SM-S928U, ...)
    pub models: &'static [&'static str],
    /// ro.product.device codenames, whole ("e3q")
    pub devices: &'static [&'static str],
    /// ro.soc.model / soc0's machine, whole, any case ("SM8650")
    pub socs: &'static [&'static str],
    /// ro.board.platform, whole, any case ("pineapple")
    pub platforms: &'static [&'static str],
    /// xnuhost settings KEY=VALUE (only keys xnuhost's config merges as device defaults)
    pub config: &'static [&'static str],
    /// environment defaults NAME=VALUE for the bridges (a variable already set wins)
    pub env: &'static [&'static str],
    /// session-mode=takeover (the opt-in "Android takeover" session, docs/booter.md 2.9d) is allowed on this phone.
    /// false: xnuhost refuses that mode at its start (exit 2) and `xnuhost --host-info` says takeover=no, which the
    /// OneOS switch reads before it stops anything. The default session (session-mode=hosted) never reads this.
    pub takeover: bool,
}

/// What was read of the phone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostFacts {
    pub model: String,
    pub device: String,
    pub platform: String,
    pub soc_model: String,
    pub soc_manufacturer: String,
    /// /sys/devices/soc0/machine, "" when absent
    pub machine: String,
    /// /sys/devices/soc0/soc_id, "" when absent
    pub soc_id: String,
    /// /proc/device-tree/compatible, its entries joined by ',' ... first ones only
    pub compatible: String,
    /// a property source was there (bionic's area or the file)
    pub props: bool,
}

const NONE: &[&str] = &[];

// ---- the table ------------------------------------------------------------------------------------------------------
// The one place that says what differs per host phone. Rules (docs/hardware.md, "The host device"):
//   - an entry holds only settings that already exist as options: an xnuhost setting that the config merges as a device
//     default (gpu, panel-fallback, hz, width/height/scale (one frame), return-graceful-ms, astc, hw-backend*), or an
//     environment variable a bridge reads;
//   - the home device (the S24 Ultra) has none: its behaviour is today's, byte for byte;
//   - takeover: true allows session-mode=takeover there (the opt-in Android takeover, docs/booter.md 2.9d): only on a
//     phone where that mode ran (the S26 Ultra, YMGPwcca's experiment) or that is the owner's (the S24 Ultra);
//   - a value the docs mark unverified on that phone is not set (today's default is the conservative one) and is said
//     here. Marks as in docs/s25.md: [F] read from the source named, [I] inferred, [U] unknown until a phone shows it.
// Match strings: the model prefixes and codenames are the phone gates' (scripts/port/verify-host.sh, docs/a56-oneos-fork.md);
// a SoC or platform match alone means "this SoC's entry" (the S24 and S24+ on SM8650 get the S24 Ultra's: no change).
static TABLE: [HostDevice; 7] = [
    // The home device. [F] SM-S928*, e3q, SM8650 (verify-host.sh, the OneOS fork's gate); platform pineapple [F] (the
    // stock device tree, qcom/pineapple-v2.dts, docs/hardware.md). No defaults: everything is today's.
    // The model prefix takes every S24 Ultra model number (docs/hardware.md 7h, "The S24 Ultra's models"; the record
    // profiles/host-galaxy-s24-ultra-sm8650.toml): SM-S928B (the owner's phone [F]), SM-S928U and SM-S928U1 (US),
    // SM-S928W (Canada, CSC XAC), SM-S928N (Korea), SM-S9280 (China).
    // The SM-S928W runs the US carrier model's bootloader and vendor partition under its own firmware (S928USQU6EZIF,
    // ro.product.vendor.model=SM-S928U [F per the OneOS fork's public issue 128]): nothing here reads either, so it is
    // matched by its own ro.product.model. e3q and SM8650 on the U, U1, W, N and 0 [I] (one hardware family; only the B
    // has booted iOS). The Japanese ones (SC-52E, SCG26) are not SM-S928 and their codename is [U]: they
    // get this entry by their SoC (SM8650, the SoC's entry), which has no defaults either.
    // takeover: session-mode=takeover is allowed (the owner's phone, 2026-11-23: the opt-in Android takeover, ported from
    // YMGPwcca/OneOS's S26 experiment; docs/booter.md 2.9d). Not run on the phone yet.
    HostDevice {
        name: "galaxy-s24-ultra",
        label: "Galaxy S24 Ultra (SM-S928, SM8650)",
        models: &["SM-S928"],
        devices: &["e3q"],
        socs: &["SM8650"],
        platforms: &["pineapple"],
        config: NONE,
        env: NONE,
        takeover: true,
    },
    // docs/s25.md "Galaxy S25 Ultra", profiles/host-galaxy-s25-ultra-sm8750.toml. [F] SM-S938, pa3q, SM8750, platform sun
    // (YumikoHayamato/OneiOS s25-ultra-sm8750: the fork's SM-S938B, One UI 9.0, sessions 1-7 on 2026-11-19).
    // ONEIOS_THERMAL_SOC_TRIPS=1, ONEIOS_THERMAL_SOC_SAMPLES=3 [F] (the same sessions): the kernel's first passive trip on
    // the SoC zones is 105 C and the hot one 125 C, so the S24's SoC rule (HEAVY 95 C, TRAPPING 105 C, one reading) put
    // iOS in TRAPPING (its "too hot" screen) at every throttle: 20 level changes in 8 minutes, 5 into TRAPPING, on single
    // junction spikes (68 -> 104 -> 68 C between polls). With the trips (115/120 C) and the lowest of 3 readings (15 s)
    // the SoC rule stayed under them in sessions 3-7; the levels came from the case, as on the S24. The SoC rule takes no
    // step (only the case and the cell do): this changes what iOS is told, not what xnuhost runs.
    // Not set: the backlight (panel0-backlight max 12750 [F], One UI's manual top not measured on this phone: the S26's
    // 2300 is not assumed), the panel (1440x3120 command mode [F]: the display helper's report gives it).
    HostDevice {
        name: "galaxy-s25-ultra",
        label: "Galaxy S25 Ultra (SM-S938, SM8750)",
        models: &["SM-S938"],
        devices: &["pa3q"],
        socs: &["SM8750"],
        platforms: &["sun"],
        config: NONE,
        env: &["ONEIOS_THERMAL_SOC_TRIPS=1", "ONEIOS_THERMAL_SOC_SAMPLES=3"],
        takeover: false,
    },
    // docs/s25.md: [F] SM-S931B, pa1q, SM8750. panel-fallback=1080x2340@120 [F] (xaxaapple/s25ver's device profile and
    // xnuhost.h: a 1080x2340 panel): only the mode assumed where the panel cannot be read (the display helper's report
    // still wins); without it such a session drew 1440x3120, 1.78 times the pixels, scaled down at every flush. Its
    // Bluetooth HAL (HIDL or AIDL) and bridge node names are [U], as for the S25 Ultra. The SoC rule as the S25 Ultra's
    // [I] (same SM8750): the thresholds come from this phone's own trips, read at the bridge's start and logged
    // ("thermal: the SoC rule: ... the kernel's trips: ..."), never under 95/105 C.
    HostDevice {
        name: "galaxy-s25",
        label: "Galaxy S25 (SM-S931, SM8750)",
        models: &["SM-S931"],
        devices: &["pa1q"],
        socs: &["SM8750"],
        platforms: &["sun"],
        config: &[
            "panel-fallback=1080x2340@120",
            // The panel's rounded corners: 34 dp in the stock ROM's own framework overlay (102 px at @3x; NoNameBuilder/OneiOS 120df342).
            "corner-radius=34",
            // Side keys and power key as built-in Consumer Control buttons (NoNameBuilder/OneiOS 23b9b85f).
            "boot-args-add=oneios-hid-buttons-builtin=1 oneios-button-haptics=1",
            // Case temperature from the stock android.hardware.thermal HAL (SKIN), with files as fallback (NoNameBuilder/OneiOS 9bed1282).
            "hw-backend-thermal=treble",
            "hw-backend-fallback=1",
        ],
        env: &["ONEIOS_THERMAL_SOC_TRIPS=1", "ONEIOS_THERMAL_SOC_SAMPLES=3"],
        takeover: false,
    },
    // docs/s26.md, profiles/host-galaxy-s26-ultra-sm8850.toml, the port-sm8850 commits. [F] SM-S948B, m3q, SM8850, platform
    // canoe (YMGPwcca/OneiOS: the fork's SM-S948B, S948BXXS4BZIG, measured 2026-11-16; port-sm8850-s26, 2026-11-23).
    // ONEIOS_BRIGHT_MAX=2300 [F]: the fork's phone with One UI in control, screen_brightness 255 (One UI's manual top)
    // wrote 2300 to panel0-backlight, whose max_brightness is 12750 (brightness's test_sm8850_manual_brightness_cap).
    // Without it the iOS slider's top writes 12750, 5.5 times One UI's manual top (HBM territory: heat and current).
    // 2300 is the conservative value, not a claim about the panel's maximum.
    // takeover: session-mode=takeover is allowed [F: the phone YMGPwcca's root-only Framework-takeover experiment ran on,
    // 2026-11-20..22, with the native touchscreen grabbed]. Not through this tree's switch yet. touch-exclusive itself is
    // not a default here: session-mode=takeover turns it on (xnuhost's config), session-mode=hosted keeps it off on every
    // phone. The native-touchscreen priority (xnuhost's input::accept_touch) and the cpu-hw-trip-* exclusion (thermal)
    // already apply on every phone.
    HostDevice {
        name: "galaxy-s26-ultra",
        label: "Galaxy S26 Ultra (SM-S948, SM8850)",
        models: &["SM-S948"],
        devices: &["m3q"],
        socs: &["SM8850"],
        platforms: &["canoe"],
        config: NONE,
        env: &["ONEIOS_BRIGHT_MAX=2300"],
        takeover: true,
    },
    // Galaxy S26 (base model, Snapdragon 8 Elite Gen 2 / SM8850; burugou05-wq/OneiOS e85405f1).
    // [F] SC-51G (docomo SM-S942D), m1q, SM8850, platform canoe, One UI 9.0 / Android 17 (6.12.69, 4 KiB pages),
    // 1080x2340@120 Hz panel (mMaxLuminance 1275.0), Oryon 8 cores, Adreno 840 KGSL + /dev/udmabuf, thermal passive
    // 105-114 C / hot 125 C. Placed after galaxy-s26-ultra so bare SM8850/canoe without a model or device keeps
    // matching galaxy-s26-ultra.
    HostDevice {
        name: "galaxy-s26",
        label: "Galaxy S26 (SM-S942 / SM-S941 / SC-51G, SM8850)",
        models: &["SM-S942", "SM-S941", "SC-51G", "SCG36"],
        devices: &["m1q"],
        socs: &["SM8850"],
        platforms: &["canoe"],
        config: &["panel-fallback=1080x2340@120"],
        env: &["ONEIOS_BRIGHT_MAX=2300", "ONEIOS_THERMAL_SOC_TRIPS=1", "ONEIOS_THERMAL_SOC_SAMPLES=3"],
        takeover: false,
    },
    // Galaxy S26 (base model, Exynos 2600 / s5e9965, m1s; burugou05-wq/OneiOS e85405f1). No KGSL on Exynos
    // (Xclipse 960 on amdgpu), so gpu=2d and FHD+ 1080x2340@120 panel fallback until an m1s unit is captured and verified.
    HostDevice {
        name: "galaxy-s26-exynos",
        label: "Galaxy S26 (SM-S942 / SM-S941, s5e9965)",
        models: &["SM-S942", "SM-S941"],
        devices: &["m1s"],
        socs: &["s5e9965"],
        platforms: &["s5e9965", "erd9965"],
        config: &["gpu=2d", "panel-fallback=1080x2340@120"],
        env: NONE,
        takeover: false,
    },
    // docs/a56.md, profiles/ios-*-a56.toml, docs/a56-oneos-fork.md. [F] SM-A566E, a56x, s5e8855 (the fork's gate:
    // ro.soc.model); platform s5e8855 [I]. gpu=auto (2026-11-24, docs/a56-gpu.md 2; gpu=2d before): no KGSL (Xclipse 540
    // on amdgpu, /dev/dri/renderD128), so xnuhost takes its amdgpu renderer (gpu3d_amdgpu.rs) only when the kernel's
    // gpuaddon carries the A56's Metal plug-in (OneiOSRadeonMetal) and the renderer's self-test passes at the start;
    // otherwise the session is 2D as before, with a "gpu: 2D only: ..." line saying which (its 8 MiB GPU pool then
    // unused). gpu=2d in the image's xnuhost.conf or on the command line keeps it 2D. panel-fallback=1080x2340@120 [F] (the fork's docs/a56-guide.md), as for the
    // S25. Touch, battery and thermal node names on the A56 are [U]: samsung stays.
    // 2026-11-24 (port/a56-full, the fork Musicalista/OneiOS-A56 to c83b9ca): the guest frame 720x1560 @2x at 60 Hz [F]
    // (c83b9ca, e8747cc: the same 360x780 points as 1080x2340 @3x, 2.25x fewer pixels for its CPU compositor; the display
    // helper scales it to the panel); return-graceful-ms=4000 [F] (e8747cc: kernel 0106 sends logd SIGTERM in that window,
    // so iOS's log reaches the Data store); astc=0 (2026-11-25): ASTC on was measured on an Adreno phone only (UniXNU-zizhan, docs/switches.md), the
    // A56 keeps its device tree as before; haptics on the AIDL vibrator HAL [I] (the A56 has no FF_PERIODIC/FF_SINE
    // evdev device: "haptics: off: no force-feedback device ..." in issue 73's log; the fork found none either), back to
    // the evdev search if the HAL is not registered (hw-backend-fallback=1: haptics is the row's only treble bridge).
    HostDevice {
        name: "galaxy-a56",
        label: "Galaxy A56 (SM-A566, Exynos 1580)",
        models: &["SM-A566"],
        devices: &["a56x"],
        socs: &["s5e8855"],
        platforms: &["s5e8855"],
        config: &[
            "gpu=auto",
            "panel-fallback=1080x2340@120",
            "width=720",
            "height=1560",
            "scale=2",
            "hz=60",
            "return-graceful-ms=4000",
            "astc=0",
            "hw-backend-haptics=treble",
            "hw-backend-fallback=1",
        ],
        env: NONE,
        takeover: false,
    },
];

/// The table, in match order (the home device first).
pub fn host_table() -> &'static [HostDevice] {
    &TABLE
}

/// An entry by its name (any case).
pub fn host_find(name: &str) -> Option<&'static HostDevice> {
    TABLE.iter().find(|e| e.name.eq_ignore_ascii_case(name))
}

/// The names, comma-separated: "galaxy-s24-ultra, galaxy-s25-ultra, ...".
pub fn host_names() -> String {
    TABLE.iter().map(|e| e.name).collect::<Vec<_>>().join(", ")
}

// ---- reading --------------------------------------------------------------------------------------------------------
const PROP_NAMES: [&str; 5] =
    ["ro.product.model", "ro.product.device", "ro.board.platform", "ro.soc.model", "ro.soc.manufacturer"];

impl HostFacts {
    fn prop_slot(&mut self, i: usize) -> &mut String {
        match i {
            0 => &mut self.model,
            1 => &mut self.device,
            2 => &mut self.platform,
            3 => &mut self.soc_model,
            _ => &mut self.soc_manufacturer,
        }
    }

    fn prop(&self, i: usize) -> &str {
        match i {
            0 => &self.model,
            1 => &self.device,
            2 => &self.platform,
            3 => &self.soc_model,
            _ => &self.soc_manufacturer,
        }
    }

    fn set_prop(&mut self, key: &str, value: &str) {
        if let Some(i) = PROP_NAMES.iter().position(|n| *n == key) {
            *self.prop_slot(i) = truncate(value.trim_ascii(), HOST_VALUE - 1).to_owned();
        }
    }
}

/// A getprop dump: "[ro.product.model]: [SM-S928B]" (adb shell getprop) or "ro.product.model=SM-S928B" (build.prop).
fn props_from_file(f: &mut HostFacts, path: &str) -> std::io::Result<()> {
    let file = std::fs::File::open(path)?;
    for line in BufReader::new(file).split(b'\n') {
        let line = String::from_utf8_lossy(&line?).into_owned();
        if let Some(rest) = line.strip_prefix('[') {
            let Some(end) = rest.find(']') else { continue };
            let (key, after) = (&rest[..end], &rest[end + 1..]);
            let Some(open) = after.find('[') else { continue };
            let Some(close) = after[open..].rfind(']') else { continue };
            f.set_prop(key, &after[open + 1..open + close]);
        } else if let Some(eq) = line.find('=') {
            if line.starts_with('#') {
                continue;
            }
            let value = &line[eq + 1..];
            f.set_prop(&line[..eq], value.split(['\r', '\n']).next().unwrap_or(""));
        }
    }
    f.props = true;
    Ok(())
}

/// Bionic's property area (a property that is not there stays "").
#[cfg(target_os = "android")]
fn props_from_bionic(f: &mut HostFacts) {
    f.props = true;
    for (i, name) in PROP_NAMES.iter().enumerate() {
        let name = std::ffi::CString::new(*name).expect("no NUL in a property name");
        let mut value = [0 as libc::c_char; HOST_VALUE];
        // SAFETY: name is NUL-terminated and value holds PROP_VALUE_MAX bytes, as __system_property_get requires.
        let n = unsafe { libc::__system_property_get(name.as_ptr(), value.as_mut_ptr()) };
        if n > 0 {
            // SAFETY: __system_property_get NUL-terminates what it wrote into value.
            let text = unsafe { std::ffi::CStr::from_ptr(value.as_ptr()) };
            *f.prop_slot(i) = text.to_string_lossy().into_owned();
        }
    }
}

/// A small file under root: at most `size - 1` bytes, NULs (a device tree's string list) as ',' when `list`, the line
/// break dropped. "" when absent.
fn read_small(root: &str, rel: &str, size: usize, list: bool) -> String {
    let Ok(mut bytes) = xh_core::fs::read_bytes(format!("{root}{rel}"), size - 1) else { return String::new() };
    while bytes.last().is_some_and(|b| matches!(b, 0 | b'\n' | b'\r')) {
        bytes.pop();
    }
    if list {
        bytes.iter_mut().filter(|b| **b == 0).for_each(|b| *b = b',');
    }
    let end = bytes.iter().position(|b| matches!(b, b'\r' | b'\n' | 0)).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Reads the facts. props_file: a getprop dump ("[key]: [value]" lines, or key=value) instead of the property area
/// ("": the property area on Android, none elsewhere). root: a folder that stands for "/" for the sysfs and proc
/// fallbacks ("": "/"). Err when props_file was given and cannot be read.
pub fn host_read(props_file: &str, root: &str) -> std::io::Result<HostFacts> {
    let mut f = HostFacts::default();
    if !props_file.is_empty() {
        props_from_file(&mut f, props_file)?;
    } else {
        #[cfg(target_os = "android")]
        props_from_bionic(&mut f);
    }
    f.machine = read_small(root, "/sys/devices/soc0/machine", 64, false);
    f.soc_id = read_small(root, "/sys/devices/soc0/soc_id", 32, false);
    f.compatible = read_small(root, "/proc/device-tree/compatible", 160, true);
    Ok(f)
}

// ---- matching -------------------------------------------------------------------------------------------------------
/// A compatible entry "vendor,NAME" whose NAME is the SoC or the platform (any case) [I]: the Linux convention
/// ("qcom,sm8650", "qcom,pineapple"); a board's own entries ("qcom,pineapple-mtp") do not match.
fn compatible_has(list: &str, name: &str) -> bool {
    let mut words = list.split(',');
    while let (Some(_vendor), Some(chip)) = (words.next(), words.next()) {
        if chip.eq_ignore_ascii_case(name) {
            return true;
        }
    }
    false
}

/// The entry the facts match and the fact that matched it: "ro.product.model=SM-S928B".
pub fn host_match(f: &HostFacts) -> Option<(&'static HostDevice, String)> {
    let any_case = |list: &[&str], value: &str| !value.is_empty() && list.iter().any(|s| s.eq_ignore_ascii_case(value));
    if !f.model.is_empty() {
        let model_matches = |e: &&HostDevice| e.models.iter().any(|m| f.model.starts_with(m));
        let hw_agrees = |e: &&HostDevice| {
            any_case(e.devices, &f.device)
                || (!f.soc_model.is_empty() && e.socs.iter().any(|s| s.eq_ignore_ascii_case(&f.soc_model)))
                || (!f.machine.is_empty() && e.socs.iter().any(|s| s.eq_ignore_ascii_case(&f.machine)))
                || any_case(e.platforms, &f.platform)
                || (!f.compatible.is_empty()
                    && (e.socs.iter().any(|n| compatible_has(&f.compatible, n))
                        || e.platforms.iter().any(|n| compatible_has(&f.compatible, n))))
        };
        if let Some(e) = TABLE.iter().filter(model_matches).find(hw_agrees).or_else(|| TABLE.iter().find(model_matches))
        {
            return Some((e, format!("ro.product.model={}", f.model)));
        }
    }
    if let Some(e) = TABLE.iter().find(|e| any_case(e.devices, &f.device)) {
        return Some((e, format!("ro.product.device={}", f.device)));
    }
    for e in &TABLE {
        for soc in e.socs {
            if !f.soc_model.is_empty() && soc.eq_ignore_ascii_case(&f.soc_model) {
                return Some((e, format!("ro.soc.model={} (the SoC's entry)", f.soc_model)));
            }
            if !f.machine.is_empty() && soc.eq_ignore_ascii_case(&f.machine) {
                return Some((e, format!("soc0.machine={} (the SoC's entry)", f.machine)));
            }
        }
    }
    if let Some(e) = TABLE.iter().find(|e| any_case(e.platforms, &f.platform)) {
        return Some((e, format!("ro.board.platform={} (the SoC's entry)", f.platform)));
    }
    if !f.compatible.is_empty() {
        let has = |list: &[&str]| list.iter().any(|n| compatible_has(&f.compatible, n));
        if let Some(e) = TABLE.iter().find(|e| has(e.socs) || has(e.platforms)) {
            return Some((
                e,
                format!("the device tree's compatible {} (the SoC's entry)", truncate(&f.compatible, 120)),
            ));
        }
    }
    None
}

/// What was read, for the log: "ro.product.model=SM-S928B ro.product.device=e3q ro.board.platform=pineapple
/// ro.soc.model=SM8650 ro.soc.manufacturer=QTI" and, when present, "soc0.machine=... soc0.soc_id=... dt=...".
pub fn host_describe(f: &HostFacts) -> String {
    let mut out = if f.props {
        (0..PROP_NAMES.len())
            .map(|i| {
                let v = f.prop(i);
                format!("{}={}", PROP_NAMES[i], if v.is_empty() { "(none)" } else { v })
            })
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        "no property area".to_owned()
    };
    if !f.machine.is_empty() {
        out += &format!(" soc0.machine={}", f.machine);
    }
    if !f.soc_id.is_empty() {
        out += &format!(" soc0.soc_id={}", f.soc_id);
    }
    if !f.compatible.is_empty() {
        out += &format!(" dt={}", truncate(&f.compatible, 120));
    }
    out
}

/// The choice for a setting (auto or "": detect; none; a name): the entry (None: none or unknown), the facts (read
/// unless the setting is none) and why: "matched by ro.product.model=SM-S928B", "host-device=galaxy-a56 (forced)",
/// "host-device=none: detection off", "unknown phone". Err(why): a name that is not in the table, a property file
/// that cannot be read.
pub fn host_choose(
    setting: &str,
    props_file: &str,
    root: &str,
) -> Result<(Option<&'static HostDevice>, HostFacts, String), String> {
    if setting.eq_ignore_ascii_case("none") {
        return Ok((None, HostFacts::default(), "host-device=none: detection off".to_owned()));
    }
    let forced = !setting.is_empty() && !setting.eq_ignore_ascii_case("auto");
    let named = if forced {
        Some(host_find(setting).ok_or_else(|| format!("host-device={setting} is not a known phone"))?)
    } else {
        None
    };
    let f = host_read(props_file, root).map_err(|_| format!("cannot read the property file {props_file}"))?;
    if let Some(e) = named {
        return Ok((Some(e), f, format!("host-device={} (forced)", e.name)));
    }
    Ok(match host_match(&f) {
        Some((e, by)) => (Some(e), f, format!("matched by {by}")),
        None => (None, f, "unknown phone".to_owned()),
    })
}

// ---- the environment's defaults -------------------------------------------------------------------------------------
fn env_pairs(entry: &HostDevice) -> impl Iterator<Item = (&'static str, &'static str, &'static str)> + '_ {
    entry.env.iter().filter_map(|pair| pair.split_once('=').map(|(name, value)| (*pair, name, value)))
}

fn env_set(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The entry's environment defaults, what each would do, for the log ("ONEIOS_BRIGHT_MAX=2300" taken, or
/// "ONEIOS_BRIGHT_MAX=2300 not taken (the environment's 1000)"), joined by ", ".
pub fn host_env_describe(entry: &HostDevice) -> String {
    env_pairs(entry)
        .map(|(pair, name, _)| match env_set(name) {
            Some(now) => format!("{pair} not taken (the environment's {})", truncate(&now, 40)),
            None => pair.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Sets the entry's environment defaults (a variable already set wins). Call it while the process has one thread.
pub fn host_env_apply(entry: &HostDevice) {
    for (_, name, value) in env_pairs(entry) {
        if env_set(name).is_none() {
            std::env::set_var(name, value);
        }
    }
}

// ---- the ctl tools --------------------------------------------------------------------------------------------------
/// The ctl tools (brightctl, batteryctl, thermalctl, bridgectl): ONEIOS_HOST_DEVICE, ONEIOS_HOST_PROPS, ONEIOS_HOST_ROOT,
/// detect, apply the entry's environment defaults, merge its hw-backend-* defaults under `hw` (which holds the
/// environment's own), and give the one line ("host: ..."). Err(line): a bad ONEIOS_HOST_DEVICE or ONEIOS_HOST_PROPS.
pub fn host_tool(hw: &mut HwSettings) -> Result<(Option<&'static HostDevice>, String), String> {
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    let setting = var("ONEIOS_HOST_DEVICE");
    let (entry, f, why) = host_choose(&setting, &var("ONEIOS_HOST_PROPS"), &var("ONEIOS_HOST_ROOT"))
        .map_err(|why| format!("host: {why} (ONEIOS_HOST_DEVICE: auto, none or one of {})", host_names()))?;
    let Some(e) = entry else {
        let line = if setting.eq_ignore_ascii_case("none") {
            "host: detection off (ONEIOS_HOST_DEVICE=none): today's defaults".to_owned()
        } else {
            format!("host: unknown phone ({}): today's defaults", host_describe(&f))
        };
        return Ok((None, line));
    };
    let env = host_env_describe(e);
    host_env_apply(e);
    // Its hw-backend-* defaults, under the environment's.
    let mut dev = HwSettings::default();
    let taken: Vec<&str> = e
        .config
        .iter()
        .copied()
        .filter(|pair| pair.starts_with("hw-backend"))
        .filter(|pair| pair.split_once('=').is_some_and(|(key, value)| dev.setting(key, value).is_ok()))
        .collect();
    let hwtext = taken.join(", ");
    hw.merge(&dev);
    let line = format!(
        "host: {}, {why}; read: {}; defaults: {}{}{hwtext}",
        e.name,
        host_describe(&f),
        if !env.is_empty() || !hwtext.is_empty() { env.as_str() } else { "none (today's)" },
        if !env.is_empty() && !hwtext.is_empty() { ", " } else { "" },
    );
    Ok((Some(e), line))
}
