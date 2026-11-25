// SPDX-License-Identifier: MIT
//! The brightness bridge: iOS's backlight level on the Galaxy S24 Ultra's panel, and the phone's ambient light sensor
//! for iOS's auto-brightness. README.md here says why each route was chosen.
//!
//! Panel: the backlight class device of Samsung's panel driver (/sys/class/backlight/panel), written as root. It needs
//! neither system_server nor surfaceflinger. Light sensor: the evdev device "als_rear" (STK6D2x), read without
//! EVIOCGRAB.
//!
//! Two backends (hw/, hw-backend-backlight): samsung, the above and the touch IC's power through Samsung's sec class,
//! unchanged; treble, the standard backlight class by named paths only ([`TREBLE_NAMES`], the whole max_brightness
//! range), no touch IC node (none is vendor-neutral: left alone), and no light sensor yet (the vendor-neutral one is
//! the sensors HAL; hw-backend-fallback=1 uses the samsung lookup for it).

mod session;
#[cfg(test)]
pub(crate) mod tests;

pub use session::{display_on, AlsFn, AlsSample, Bright, Info, Options, TraceFn};

/// The level scale of the bridge channel (struct ob_bright_set; drivers/brightness/oneios_bl_request.h's
/// OBL_FRACTION_FULL).
pub const FULL: u32 = 65535;
/// The treble backend's backlight class devices, tried in this order (each DIR/NAME/max_brightness, a named file).
pub const TREBLE_NAMES: [&str; 5] = ["panel0-backlight", "panel1-backlight", "backlight", "lcd-backlight", "panel"];

// ---- the curve (pure) ---------------------------------------------------------------------------

/// The slider's top on the panel's scale. Samsung's node goes past the manual range when it also carries the
/// high-brightness levels (max_brightness 510 = 2 x 255, 51000 = 2 x 25500): those are left to One UI, and the slider
/// ends at the manual maximum.
pub fn ceiling(max_brightness: u32) -> u32 {
    if max_brightness == 510 || max_brightness == 51000 {
        max_brightness / 2
    } else {
        max_brightness
    }
}

/// iOS's level (0..[`FULL`]) to the node's value in floor..ceiling. A perceived level (the slider's "brightness") is
/// squared, as on the Mi A3 port; a linear one (a fraction of the nits) is taken as it is. Level 0 gives the floor,
/// never 0: the panel is not turned off from here.
pub fn map(level: u32, linear: bool, floor: u32, ceiling: u32) -> u32 {
    if ceiling <= floor {
        return ceiling;
    }
    let (span, x, full) = (u64::from(ceiling - floor), u64::from(level.min(FULL)), u64::from(FULL));
    let part = if linear { (span * x + full / 2) / full } else { (span * x * x + full * full / 2) / (full * full) };
    floor + part as u32
}

/// Where a fade from `from` to `to` is after elapsed_ms of fade_ms (both on the level scale).
pub fn fade_at(from: u32, to: u32, elapsed_ms: u32, fade_ms: u32) -> u32 {
    if fade_ms == 0 || elapsed_ms >= fade_ms {
        return to;
    }
    let delta = i64::from(to) - i64::from(from);
    (i64::from(from) + delta * i64::from(elapsed_ms) / i64::from(fade_ms)) as u32
}

/// The level whose squared map is nearest to a node value: where a first fade starts from.
fn level_of(value: u32, floor: u32, ceiling: u32) -> u32 {
    if ceiling <= floor || value <= floor {
        return 0;
    }
    if value >= ceiling {
        return FULL;
    }
    let full = u64::from(FULL);
    let square = u64::from(value - floor) * full * full / u64::from(ceiling - floor);
    let mut root = 0u64;
    let mut bit = 1u64 << 16;
    while bit != 0 {
        if (root + bit) * (root + bit) <= square {
            root += bit;
        }
        bit >>= 1;
    }
    root as u32
}

// ---- the light sensor's events (pure) -----------------------------------------------------------

// The kernel's input codes (linux/input-event-codes.h).
use xh_core::evdev::codes::{ABS_RX, ABS_Y, ABS_Z, EV_ABS, EV_REL, EV_SYN, REL_RX, REL_RY, REL_X, SYN_REPORT};

/// One report of the STK6D2x driver (drivers/optics/stk6d2x_sec.c, stk_sec_report): the averages of the clear,
/// infrared and UV channels, already divided by the gain, each sent as value + 1 on EV_REL (REL_RY clear, REL_X
/// infrared, REL_RX UV), then the gains on EV_ABS and EV_SYN.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AlsFrame {
    /// Counts (the event's value - 1).
    pub clear: u32,
    pub ir: u32,
    pub uv: u32,
    /// As last reported (value - 1); 0 = not seen.
    pub clear_gain: u32,
    pub ir_gain: u32,
    pub uv_gain: u32,
    pub have_clear: bool,
}

fn minus_one(value: i32) -> u32 {
    if value > 0 {
        value as u32 - 1
    } else {
        0
    }
}

impl AlsFrame {
    /// Takes one input event. True when it ends a report that carried a clear value.
    pub fn feed(&mut self, kind: u16, code: u16, value: i32) -> bool {
        match (kind, code) {
            (EV_REL, REL_RY) => {
                self.clear = minus_one(value);
                self.have_clear = true;
            }
            (EV_REL, REL_X) => self.ir = minus_one(value),
            (EV_REL, REL_RX) => self.uv = minus_one(value),
            (EV_ABS, ABS_Y) => self.clear_gain = minus_one(value),
            (EV_ABS, ABS_Z) => self.ir_gain = minus_one(value),
            (EV_ABS, ABS_RX) => self.uv_gain = minus_one(value),
            (EV_SYN, SYN_REPORT) => return std::mem::take(&mut self.have_clear),
            _ => {}
        }
        false
    }

    /// Clear counts to millilux. millilux_per_count is the calibration (README.md); saturates at 2^32-1.
    pub fn millilux(&self, millilux_per_count: u32) -> u32 {
        u32::try_from(u64::from(self.clear) * u64::from(millilux_per_count)).unwrap_or(u32::MAX)
    }
}

/// Whether a new value is worth a message to the guest: a change of a tenth (or 1 lux), not more often than every
/// 200 ms; a repeat every 2 s.
pub fn als_changed(last_millilux: u32, now_millilux: u32, ms_since_last: u32) -> bool {
    if ms_since_last >= 2000 {
        return true;
    }
    if ms_since_last < 200 {
        return false;
    }
    last_millilux.abs_diff(now_millilux) >= (last_millilux / 10).max(1000)
}
