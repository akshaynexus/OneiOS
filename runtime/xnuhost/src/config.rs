// SPDX-License-Identifier: MIT
//! Port of config.c: the command line, the environment and the image set's optional xnuhost.conf (docs/booter.md).
//!
//! [`Config::new`] holds the built-in defaults; [`Config::set`] applies one `KEY=VALUE`; [`Config::file`] a file of
//! them; [`Config::args`] the ONEIOS_* environment and argv; [`Config::finish`] merges the image set's settings (the
//! command line wins), the host phone's defaults (under both), and fills what depends on other values: the kernel, the
//! disks, the guest's RAM (ram=auto and what fits).
//!
//! What C kept as -1 ("unset") is a plain value here plus the set of keys given ([`Config::given`]); the values that
//! stay unset after the finish (the module decides) are `Option`s.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// xh_parse_size, xh_meminfo_value, xh_proc_values: xh-core's (the same rules; a size that overflows is refused).
pub use xh_core::fs::{meminfo_value, proc_values};
use xh_core::log::Level;
use xh_core::parse;
pub use xh_core::parse::size as parse_size;
use xnuloader::machine::CAMERA_MODEL;

use onebridges::hw::{self as bhw, Backend, Bridge, HostDevice, HwSettings};

/// XH_MAX_CPUS: the vCPUs of a guest at most (the phone has 8 cores).
pub const MAX_CPUS: usize = 8;
/// XH_MAX_DISKS.
pub const MAX_DISKS: usize = 6;

/// boot-args holds at most this many bytes with its NUL (xnuhost.h's `char boot_args[1024]`).
pub const BOOT_ARGS_SIZE: usize = 1024;
/// The text fixups' list at most, the NULs included (`char text_fixups[1024]`).
pub const TEXT_FIXUPS_SIZE: usize = 1024;

// ---- the RAM -----------------------------------------------------------------------------------------------------
const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
const STEP_256M: u64 = 256 * MIB;

/// ram-view=auto: an iPhone18,2's DRAM.
pub const RAM_VIEW_AUTO: u64 = 12 << 30;
/// ram=auto (docs/booter.md 1): min(percent % of total, total - floor), rounded down to 256 MiB, at most RAM_MAX.
pub const RAM_AUTO_PERCENT: u32 = 90;
pub const RAM_AUTO_FLOOR: u64 = 3 << 29; // 1.5 GiB
/// ram-session-reserve: the host's MemAvailable that what fits leaves at the session's peak, after the guest, its tasks
/// and the GPU took theirs (docs/booter.md 2.2, "What fits"). With the RAM locked (the default since fix-ram-locked,
/// 2026-11-15, where the lock can work: CAP_IPC_LOCK or no RLIMIT_MEMLOCK) 1536 MiB, from the phone's two sessions of
/// 2026-11-23 (18:24 and 19:04, 5632 MiB locked): the host swapped in 31-42 MB/s at MemAvailable 1077-1265 MiB (its own
/// working set: surfaceflinger, the helpers, xnuhost's code, the image's page cache) and under 0.2 MB/s from 1503 MiB
/// up. It holds the valve's level, the way back and that working set. Unlocked 256 MiB more: the host may swap the
/// guest, and zram's pool and working set for it come on top.
pub const RAM_SESSION_RESERVE: u64 = 1792 << 20;
pub const RAM_SESSION_RESERVE_LOCKED: u64 = 1536 << 20;
/// ram-unlock-below: the lock's valve.
pub const RAM_UNLOCK_BELOW: u64 = 256 << 20;
/// With the lock the valve is never lower (0 included): the way back must fit.
pub const RAM_UNLOCK_MIN: u64 = 128 << 20;
/// What fits leaves at least the valve's level and this much.
pub const RAM_VALVE_MARGIN: u64 = 256 << 20;
/// ram-gpu-reserve: KGSL's own memory and what the helpers hold outside the guest. The GPU's blobs are not in it: every
/// blob is a udmabuf over the guest's RAM memfd (docs/gpu.md 2, "No separate GPU pool"), so the guest's size bounds
/// them. The phone, 2026-11-23 19:06: host blobs 1633 MiB, KGSL imported_mem 1640 MiB (the same pages), KGSL's own
/// kernel memory 1.7 MiB; the blobs' udmabuf page arrays, sg tables and IOMMU tables are about 1 % of them [estimate].
pub const RAM_GPU_RESERVE: u64 = 256 << 20;
/// ram-pagetable-percent: page tables as a share of the guest's RAM, on top of the per-task cost. 0 since 2026-11-23:
/// the tasks' page tables follow the tasks, not the RAM (RAM_TASK_COST). Was 8 (575 MiB for 7263 MiB, 2026-11-14).
pub const RAM_PAGETABLE_PERCENT: u32 = 0;
/// ram-hold-percent: what holding the guest's RAM costs the host beyond its own bytes, % of it: MemAvailable at the
/// start less the guest's size less MemAvailable once it is held and locked. The phone, 2026-11-23: 8561 - 5632 - 2540
/// = 389 MiB (6.9 %) and 8445 - 5632 - 2480 = 333 MiB (5.9 %). Measured at one size; that it scales with the size is
/// a guess.
pub const RAM_HOLD_PERCENT: u32 = 7;
/// ram-tasks: the iOS tasks the session is sized for (each one a Linux process). The phone, 2026-11-23: most 354 at
/// 19:06 and 346 at 18:30, with 5632 MiB.
pub const RAM_TASKS: u32 = 400;
/// ram-task-cost: the host's memory for one iOS task outside the guest's RAM: its page tables over the memfd and its
/// kernel memory (its maps' VMAs, its threads' stacks and task structures). The phone, 2026-11-23: MemAvailable fell
/// 1463 MiB after the lock (2540 -> 1077) while the host swapped out 277 MiB more (zram, about 185 MiB of RAM at 3:1
/// [guess]): 1648 MiB for 353 tasks, 4.7 MiB each, of which page tables 728 MiB (60 -> 788), 2.1 MiB each; at 18:24
/// 1567 MiB for 342 tasks, 4.6 MiB each (page tables 683 MiB). Less the 256 MiB of ram-gpu-reserve (which covers the
/// non-task part): 3.9 MiB each; 4 MiB.
pub const RAM_TASK_COST: u64 = 4 << 20;
/// ram-fit-terms=old (docs/switches.md): the terms before 2026-11-23 (3b8742a): the headroom 768 MiB with the lock, 1 GiB
/// without, page tables 8 % of the guest, no per-task cost, no hold share. Each of those keys given still wins.
pub const RAM_SESSION_RESERVE_LOCKED_OLD: u64 = 768 << 20;
pub const RAM_SESSION_RESERVE_OLD: u64 = 1 << 30;
pub const RAM_PAGETABLE_PERCENT_OLD: u32 = 8;
/// gpu-blob-entries: the guest 3D driver's MaxBlobEntries, (1 MiB - 56) / 16 (virtio_gpu.rs MAX_BLOB_ENTRIES, 78c4b86).
pub const GPU_BLOB_ENTRIES: u32 = ((1 << 20) - 56) / 16;
/// ram-tasks-awake: with task-trim (task.rs, "The trim"; docs/booter.md 2.6), the tasks counted at ram-task-cost; the
/// rest of ram-tasks are trimmed (no thread released for task-trim-idle seconds: their host mappings, page tables and
/// VMAs dropped) and counted at ram-task-idle-cost. An estimate (2026-11-23): no phone log yet counts the tasks that
/// ran in 30 s; the `tasks: trim:` line counts them from the next session on, and the trim's pressure mode
/// (TRIM_PRESSURE_MARGIN) takes every task idle 2 s when the host gets near the headroom.
pub const RAM_TASKS_AWAKE: u32 = 150;
/// ram-task-idle-cost: a trimmed task's host memory: its threads' kernel stacks and task structures, its stub region's
/// maps and tables, its mm. An estimate (2026-11-23) that also keeps room in case part of the 900 MiB no line named on
/// 2026-11-23 is not the tasks' VMAs (then the trim does not give it back): 1 MiB.
pub const RAM_TASK_IDLE_COST: u64 = 1 << 20;
/// task-trim-idle: seconds without a release before a task's host mappings are dropped (task.rs, "The trim").
pub const TASK_TRIM_IDLE: u32 = 30;
/// Under pressure (MemAvailable under ram-session-reserve and this much) the trim takes every task idle
/// TASK_TRIM_IDLE_PRESSED seconds.
pub const TRIM_PRESSURE_MARGIN: u64 = 512 << 20;
pub const TASK_TRIM_IDLE_PRESSED: u32 = 2;
/// The host's pressure line (docs/booter.md 2.2, "The host's pressure"): swap-ins at or over this many bytes a second
/// over 5 s start an event (the phone, 2026-11-23: 31-42 MB/s at the end, 0.1-2.5 MB/s before).
pub const PRESSURE_SWAP_IN: u64 = 4 << 20;
/// An event ends after this many seconds with MemAvailable at least the headroom and PRESSURE_CALM_MARGIN, and
/// swap-ins under a quarter of PRESSURE_SWAP_IN.
pub const PRESSURE_CALM_S: u64 = 30;
pub const PRESSURE_CALM_MARGIN: u64 = 128 << 20;
/// The RAM window and the kernel's own range share 0x20_0000_0000 .. 0x3c_0000_0000.
pub const RAM_MAX: u64 = 64 << 30;
/// The host swaps the guest's RAM: said once past 64 MiB, then at each further 256 MiB.
pub const SWAP_NOTE_FIRST: u64 = 64 << 20;
pub const SWAP_NOTE_STEP: u64 = 256 << 20;

// ---- the display -------------------------------------------------------------------------------------------------
/// The Galaxy S24 Ultra's panel, where the host's cannot be read.
pub const PANEL_WIDTH: u32 = 1440;
pub const PANEL_HEIGHT: u32 = 3120;
pub const PANEL_HZ: u32 = 120;
/// ready-content-wait's default: an iOS whose frames are all black is ready this many seconds after its first one
/// (ready.rs). Under the switch's dead-man timer (600 s from the start; the phone's first frame came at +133 s).
pub const READY_CONTENT_WAIT: u32 = 120;

/// A setting's value with a fixed set of names: parsed from and printed as exactly the C's texts.
macro_rules! named {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $v:ident = $text:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $($(#[$vm])* $v),+ }
        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$v),+];
            /// The setting's text.
            pub fn as_str(self) -> &'static str { match self { $($name::$v => $text),+ } }
            /// The value a text names; None for any other.
            pub fn parse(text: &str) -> Option<Self> { Self::ALL.iter().copied().find(|v| v.as_str() == text) }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }
    };
}

named!(
    /// wifi-joined-scan= (docs/switches.md): cache (b704922: within 120 s of the last scan its networks answer, else a
    /// low-priority scan), low (cbf97ea: every scan the radio's, low priority), plain (no flags, as before cbf97ea).
    WifiJoinedScan { Cache = "cache", Low = "low", Plain = "plain" }
);
named!(
    /// encoder-10bit= (docs/switches.md): main10 (b3da193: P010 taken, HEVC Main10 where offered), 8bit (P010 taken,
    /// coded 8-bit), nv12 (no P010: the guest sends NV12).
    Encoder10 { Main10 = "main10", EightBit = "8bit", Nv12 = "nv12" }
);
named!(
    /// decoder= (docs/switches.md, 2026-11-24): auto (the video device decodes too: the phone's MediaCodec for iOS's
    /// paravirtualized VideoToolbox decoder; needs encoder=auto or sim, whose device and media pool it shares) | off (an
    /// encoder only: iOS decodes video in software, as before).
    Decoder { Auto = "auto", Off = "off" }
);
named!(
    /// gpu-down= (docs/switches.md): governor (3b36ff0: the level comes down when a frame fits at the governor's clock
    /// in a burst) or bottom (when it would fit at KGSL's lowest clock, as before 3b36ff0).
    GpuDown { Governor = "governor", Bottom = "bottom" }
);
named!(
    /// uclamp-rule= (docs/switches.md): work (03d85ac: xnuhost alone sets a vCPU thread's clamp from its work, 0 at
    /// idle; drm2kgsl's boost only for the frame's threads) or all (before it: the boost on every vCPU thread and every
    /// iOS thread not background; the cpufreq device sets the vCPU thread's clamp).
    UclampRule { Work = "work", All = "all" }
);
named!(
    /// stop-loop= (docs/switches.md): wait (a3077a9, fork 1d9b58f: the fork's stop loop waits on init's properties
    /// through xnuhost --prop-wait) or poll (one getprop look a second). The fork reads it; xnuhost only checks it.
    StopLoop { Wait = "wait", Poll = "poll" }
);
named!(
    /// mode=: "takeover" (the OneOS session) or "bench" (a shell, no session files).
    Mode { Takeover = "takeover", Bench = "bench" }
);
named!(
    /// session-mode=: hosted (the default: the session as before session-mode existed, byte for byte) or takeover (the
    /// opt-in Android takeover, docs/booter.md 2.9d: mode=takeover, input=1 and a host-table row that allows it; it
    /// turns the native touchscreen's grab on unless touch-exclusive= was given).
    SessionMode { Hosted = "hosted", Takeover = "takeover" }
);
named!(
    /// display=
    DisplayBackend { Shm = "shm", Drm = "drm", None = "none" }
);
named!(
    /// display-format=: the frame's pixels. bgrx (the default, XH_PIXELS_BGRX8) or rgba16f (half floats, wide colour
    /// and EDR kept; the guest is told oneios-display-wide=1).
    DisplayFormat { Bgrx8 = "bgrx", Rgba16f = "rgba16f" }
);
named!(
    /// gpu=: auto (3D when the host has KGSL, udmabuf and a dma heap; on an amdgpu host without KGSL, the amdgpu
    /// renderer when the gpuaddon carries OneiOSRadeonMetal and its self-test passes), 3d (or do not start), 2d.
    Gpu { Auto = "auto", ThreeD = "3d", TwoD = "2d" }
);
named!(
    /// gpu-level=: deadline (default: the top level only for frames that would miss), hold (every render-server frame
    /// at the top, as before 2026-11-15), governor (never asked); docs/gpu.md 2.2b.
    GpuLevel { Deadline = "deadline", Hold = "hold", Governor = "governor" }
);
named!(
    /// bluetooth=: auto (hidl when the bridges are on and the phone's HAL is registered, else virtual), virtual (no
    /// controller, transport 10), hidl (the phone's controller, transport 1, H4) or sim (a stand-in controller,
    /// bridges/bt's hci_sim, transport 1: a boot with no phone; works with the bridges off).
    Bluetooth { Auto = "auto", Virtual = "virtual", Hidl = "hidl", Sim = "sim" }
);
named!(
    /// haptics=: auto (the phone's force-feedback device when the bridges are on), off (no actuator, no Actuator audio
    /// device, no /product/haptics) or sim (a stand-in actuator that counts and logs; works with the bridges off).
    Haptics { Auto = "auto", Off = "off", Sim = "sim" }
);
named!(
    /// wifi=: auto (the phone's wlan0 for iOS's en0 when the bridges are on), off (no network device), tap (the old
    /// route: a tap and bin/oneios-net.sh's NAT), cell (a made-up joined network over that same tap, so iOS's data
    /// runs on the phone's uplink — mobile data included — with no Wi-Fi; /dev/tun and root), sim (a made-up radio
    /// with two networks; works with the bridges off).
    Wifi { Auto = "auto", Off = "off", Tap = "tap", Cell = "cell", Sim = "sim" }
);
named!(
    /// pen=: off (the default until a phone session proves it) | auto: the host's pen digitizer (pen.rs's lookup) or
    /// the script's pen as the guest's pen device /arm-io/input-1, and oneios-pen=1.
    Pen { Off = "off", Auto = "auto" }
);
named!(
    /// sensors=: off (no socket opened, as before) | auto (the default since 2026-11-25): the phone's sensor hub (SSC) over QRTR when
    /// its service 400 answers (runtime/bridges/sensors): the accelerometer, gyroscope and magnetometer as the guest's
    /// motion device /arm-io/input-N with oneios-motion=7 (15 with a magnetometer), the front light sensor for the
    /// brightness bridge with oneios-als-spu=1; else the old path (no motion device, the rear light sensor) | ssc: the same,
    /// but no rear light sensor when the hub has no light.
    Sensors { Off = "off", Auto = "auto", Ssc = "ssc" }
);
named!(
    /// pen-rotate=: the digitizer's frame turned clockwise onto the panel's; auto from the two aspects.
    PenRotate { Auto = "auto", R0 = "0", R90 = "90", R180 = "180", R270 = "270" }
);
named!(
    /// encoder=: off (the default until a phone session proves it) | auto: the video encoder bridge
    /// (runtime/bridges/encoder), /arm-io/virtio-media-0 with a media-host-pool: the phone's MediaCodec through the
    /// oneios-encoder helper in a session (the bridges on), the stand-in encoder inside xnuhost with the bridges off (a
    /// PC boot) | sim: the stand-in always.
    Encoder { Off = "off", Auto = "auto", Sim = "sim" }
);
named!(
    /// camera-vm=: hide (the default) | show: whether cameracaptured is told it runs in a VM. hide adds cameracaptured
    /// to the boot arguments' owner-hide-vmm= when the camera helper lists a camera: 24A446's CMCapture drops every
    /// back stream in a VM (FigCaptureIsRunningInVirtualization, kern.hv_vmm_present; docs/hardware.md 7). show: the VM
    /// path as before 2026-11-23.
    CameraVm { Hide = "hide", Show = "show" }
);

impl DisplayFormat {
    /// include/xnuhost/pixels.h's XH_PIXELS_*.
    pub fn pixels(self) -> u32 {
        match self {
            DisplayFormat::Bgrx8 => 1,
            DisplayFormat::Rgba16f => 2,
        }
    }
}

impl PenRotate {
    /// The turn in degrees; None for auto.
    pub fn degrees(self) -> Option<u32> {
        match self {
            PenRotate::Auto => None,
            PenRotate::R0 => Some(0),
            PenRotate::R90 => Some(90),
            PenRotate::R180 => Some(180),
            PenRotate::R270 => Some(270),
        }
    }
}

/// cpu-pin=: a vCPU and the iOS thread it runs share one host CPU of the vCPU's mask (On, the default); Always (2) also
/// with no CPU plan (a bench run): vCPU i on host CPU i.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuPin {
    Off,
    On,
    Always,
}

/// What decided ram=auto's size (XH_RAM_BOUND_*).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RamBound {
    Share = 1,
    Floor = 2,
    Available = 3,
}

/// One disk: disk=FILE (read-only), disk-rw=FILE[,SEED], data-store=FILE[,SEED].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiskConfig {
    pub path: String,
    pub writable: bool,
    /// the "droidvm,data-store" window
    pub data_store: bool,
    /// a read-only image under a writable store ("" = none)
    pub seed: String,
}

/// A mode of the panel or of the guest's display.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PanelMode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
}

/// The host phone's table entry, compared by identity (the table is static).
#[derive(Clone, Copy)]
pub struct Host(pub &'static HostDevice);
impl PartialEq for Host {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}
impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Host({})", self.0.name)
    }
}

/// text-fixup=FILE,OFFSET,ORIGINAL,REPLACEMENT entries for /chosen text-fixups: at most TEXT_FIXUPS_SIZE bytes with
/// a NUL after each, as the kernel reads them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextFixups(Vec<String>);
impl TextFixups {
    /// The bytes of the NUL-separated list.
    pub fn len_bytes(&self) -> usize {
        self.0.iter().map(|f| f.len() + 1).sum()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn entries(&self) -> &[String] {
        &self.0
    }
    /// Appends one entry when it fits in `size` bytes with the NULs; false when not.
    pub fn push_within(&mut self, entry: &str, size: usize) -> bool {
        if self.len_bytes() + entry.len() + 1 > size {
            return false;
        }
        self.0.push(entry.to_owned());
        true
    }
    /// The list as /chosen text-fixups holds it: each entry and its NUL.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len_bytes());
        for f in &self.0 {
            out.extend_from_slice(f.as_bytes());
            out.push(0);
        }
        out
    }
}

/// A value [`Config::set`] does not take (xh_config_set's -1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadSetting;

/// Why the command line or the finish failed: the lines xnuhost prints to stderr (each after "xnuhost: "), and
/// whether the usage text follows them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigError {
    pub lines: Vec<String>,
    pub usage: bool,
}
impl ConfigError {
    fn new(line: impl Into<String>) -> Self {
        ConfigError { lines: vec![line.into()], usage: false }
    }
    fn silent() -> Self {
        ConfigError::default()
    }
    fn with_usage(mut self) -> Self {
        self.usage = true;
        self
    }
    /// Prints it as xnuhost did: each line after "xnuhost: ", then the usage when asked.
    pub fn print(&self) {
        for line in &self.lines {
            eprintln!("xnuhost: {line}");
        }
        if self.usage {
            eprint!("{USAGE}");
        }
    }
}
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.lines.join("; "))
    }
}
impl std::error::Error for ConfigError {}

/// The boot arguments have no room for what is added (xh_*_boot_args' -1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoRoom;

pub const USAGE: &str = "usage: xnuhost --image DIR [--kernel FILE] [--data DIR] [--session DIR] [--runtime DIR]\n\
\x20              [--session-abi 1] [--mode takeover|bench] [--cpus N] [--ram SIZE|auto]\n\
\x20              [--boot-args TEXT] [--display shm|drm|none] [--gpu auto|3d|2d] [--width W --height H]\n\
\x20              [--hz N] [--scale 2|3] [--display-format bgrx|rgba16f]\n\
\x20              [--hw-backend samsung|treble] [--hw-backend-BRIDGE samsung|treble] [--hw-backend-fallback 0|1]\n\
\x20              [--set haptics-native-impacts=0|1] (1: needs hw-backend-haptics=treble, hw-backend-fallback=0)\n\
\x20              [--host-device auto|none|NAME] [--host-props FILE] [--host-root DIR]\n\
\x20              [--no-input] [--input-script FILE] [--set touch-exclusive=1] [--no-tasks] [--config FILE] [--set KEY=VALUE] [-v]\n\
\x20              [--set session-mode=hosted|takeover]\n\
\x20      xnuhost --selftest      checks of this host (trap layer, PAC, memory), no kernel\n\
\x20      xnuhost --host-info [--host-device ...] [--host-props FILE]   this phone's row of the host table, one line\n\
docs/booter.md describes the image set's files, the session files and the exit codes.\n";

/// `field` from the image set where the command line did not give `key`.
macro_rules! take {
    ($c:expr, $image:expr, $key:literal, $field:ident) => {
        if !$c.given($key) && $image.given($key) {
            $c.$field = $image.$field.clone();
        }
    };
}

/// The keys whose being given decides a merge or a default ([`Config::given`]).
const GIVEN_KEYS: &[&str] = &[
    "gpu",
    "gpu-level",
    "gpu-priority",
    "ram",
    "ram-view",
    "ram-percent",
    "ram-host-floor",
    "host-mem-log",
    "usb-log",
    "ram-giveback",
    "ram-ondemand",
    "ram-prefault",
    "ram-lock",
    "ram-session-reserve",
    "ram-fit",
    "ram-huge",
    "ram-gpu-reserve",
    "ram-pagetable-percent",
    "ram-hold-percent",
    "ram-tasks",
    "ram-task-cost",
    "ram-fit-terms",
    "ram-unlock-below",
    "return-graceful-ms",
    "ready-content-wait",
    "display",
    "display-format",
    "panel-fallback",
    "scale",
    "scale-fixup",
    "corner-radius",
    "artwork-subtype",
    "ios16-fixup",
    "display-direct",
    "touch-exclusive",
    "session-mode",
    "takeover-stops",
    "takeover-reclaim",
    "takeover-swappiness",
    "pen",
    "pen-rotate",
    "pen-mirror",
    "sensors",
    "sensors-hz",
    "sensors-light",
    "encoder",
    "camera-vm",
    "camera-model",
    "bridges",
    "wifi",
    "bluetooth",
    "haptics",
    "haptics-native-impacts",
    "setway-skip",
    "blk-late-flush",
    "perf-log",
    "spin-host",
    "spin-task",
    "cpu-pin",
    "map-defer",
    "wifi-ring-wait",
    "wifi-send-wait",
    "wifi-buffers",
    "wifi-joined-scan",
    "encoder-10bit",
    "encoder-warmup",
    "display-post",
    "display-present",
    "decoder",
    "decoder-codecs",
    "astc",
    "gpu-blob-entries",
    "gpu-down",
    "gpu-kgsl-ui-rb",
    "gpu-off-level",
    "stop-loop",
    "uclamp-rule",
    "task-trim",
    "task-trim-idle",
    "timer-coalesce",
    "thermal-cap",
    "ram-tasks-awake",
    "ram-task-idle-cost",
];

/// xnuhost's settings. Before [`Config::finish`] they are what was set; after it, what the session uses.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    given: BTreeSet<&'static str>,
    /// mode= or display= with a name xnuhost does not have: the finish says so (as C did)
    unsupported_mode: Option<String>,
    unsupported_display: Option<String>,

    pub session_abi: i32,
    pub mode: Mode,
    pub runtime: String,
    pub image: String,
    pub data: String,
    pub session: String,
    /// default <image>/kernel.kc (or kernel.macho)
    pub kernel: String,
    /// decoded device tree of the device, default <image>/identity.adt if present ("" none)
    pub identity: String,
    pub boot_args: String,
    /// boot-args-add: boot arguments added to boot_args, whichever gave those (the command line, the image set, the
    /// default), each unless boot_args names it already; the command line's boot-args-add wins over the image set's.
    pub boot_args_add: String,
    pub ncpu: u32,
    /// ram=SIZE or ram=auto (0, the default). The finish resolves auto from ram_meminfo: ram_bytes is then the guest's
    /// size and ram_auto_total/available what was read (both 0: not auto).
    pub ram_bytes: u64,
    /// ram-view=SIZE|auto|off (off; auto = 12 GiB): the DRAM iOS is told it has (hw.memsize_physical, the per-process
    /// limits of /defaults) and the compressor's room for it, over the same locked RAM (docs/booter.md 2.2, "The memory
    /// view"). 0 = off.
    pub ram_view: u64,
    /// ram-percent=: the guest's share of MemTotal for ram=auto, 10-100 (90)
    pub ram_percent: u32,
    /// ram-host-floor=: what ram=auto always leaves the host, bytes (1.5 GiB)
    pub ram_host_floor: u64,
    /// ram-meminfo=FILE: where ram=auto reads MemTotal (tests); default /proc/meminfo
    pub ram_meminfo: String,
    pub ram_auto_total: u64,
    pub ram_auto_available: u64,
    pub ram_auto_bound: Option<RamBound>,
    /// The RAM held fixed for the session (ramfix.rs). ram-giveback=1 is 0058's way instead: nothing prefaulted or
    /// locked, free pages given back (and ram=auto as before, without the MemAvailable bound). 0 since fix-ram-locked,
    /// 2026-11-15: the RAM held fixed and locked.
    pub ram_giveback: bool,
    /// ram-ondemand=0|1 (0; option C of 2026-11-23, the owner's to choose, against the rule that iOS's RAM is
    /// fixed): the guest's RAM is not prefaulted; each page is locked as it is first touched (mlock2 MLOCK_ONFAULT, so
    /// the host never swaps it) and iOS's free pages are given back (ram-giveback=1's way). A guest larger than what
    /// fits is backed only as iOS uses it; when it fills, the valve or the host's OOM killer ends it (docs/booter.md
    /// 2.2, "On demand").
    pub ram_ondemand: bool,
    /// ram-prefault=0|1 (1)
    pub ram_prefault: bool,
    /// ram-lock=0|1 (1): mlock the prefaulted RAM where Linux allows and it fits
    pub ram_lock: bool,
    /// the lock can work here (ramfix::can_lock: CAP_IPC_LOCK, or no RLIMIT_MEMLOCK) and is asked
    pub ram_lock_expected: bool,
    /// ram-session-reserve=SIZE (1536 MiB with the lock expected, else 1792 MiB): MemAvailable that ram=auto and the
    /// lock leave the host at the session's peak
    pub ram_session_reserve: u64,
    /// ram-unlock-below=SIZE (256 MiB): the valve; with the lock at least RAM_UNLOCK_MIN
    pub ram_unlock_below: u64,
    /// What fits (2026-11-14): ram=auto and the lock take no more than ram_fit of MemAvailable at the start.
    /// ram-fit=0|1 (1); 0: ram=auto as before 2026-11-14 (share and floor only)
    pub ram_fit_on: bool,
    /// ram-huge=0|1 (1): 2 MiB host pages for the RAM where the host allows them (fix-ram-hugepages)
    pub ram_huge: bool,
    /// ram-gpu-reserve=SIZE (256 MiB): KGSL's own memory and the GPU's host buffers
    pub ram_gpu_reserve: u64,
    /// ram-pagetable-percent=N (0): page tables, % of the guest's RAM, on top of ram-task-cost
    pub ram_pagetable_percent: u32,
    /// ram-hold-percent=N (7): what holding the guest's RAM costs the host beyond its bytes, % of it
    pub ram_hold_percent: u32,
    /// ram-tasks=N (400): the iOS tasks the session is sized for
    pub ram_tasks: u32,
    /// ram-task-cost=SIZE (4 MiB): the host's memory for one iOS task outside the guest (page tables, kernel memory)
    pub ram_task_cost: u64,
    /// ram-fit-terms=tasks|old (tasks): old is the fit of before 2026-11-23 (RAM_SESSION_RESERVE_LOCKED_OLD ...)
    pub ram_fit_terms_old: bool,
    /// ram-tasks-awake=N (150) and ram-task-idle-cost=SIZE (1 MiB): with task-trim, the tasks counted at ram-task-cost
    /// and what each of the others (trimmed) costs
    pub ram_tasks_awake: u32,
    pub ram_task_idle_cost: u64,
    /// what fits and the MemAvailable it came from (both 0: not known)
    pub ram_fit: u64,
    pub ram_fit_available: u64,
    /// The most RAM that can be held locked beside the session's needs, in 256 MiB steps: ram_fit rounded down when the
    /// lock is expected to work (ram_lock_expected), else 0. The one figure a mode that puts more behind the locked
    /// part (iOS's own compressor, fix-ios-compressor) reads; ram=auto with the lock takes min(this, share, floor).
    pub ram_lock_fit: u64,
    /// host-mem-log=SECONDS: the host's MemAvailable in the log every so often; 0 = off (30)
    pub host_mem_log: u32,
    /// usb-log=SECONDS: how often the USB gadget is read for its "usb:" line (usb.rs), which is logged at the start and
    /// on every change; 0 = off (3)
    pub usb_log: u32,
    /// return-graceful-ms: on a return (a stop signal), how long to let iOS shut itself down before the kill; 0 = the
    /// immediate kill (the default until the guest half is tested)
    pub return_graceful_ms: u32,
    /// ready-content-wait=SECONDS: session/ready comes at the guest's first frame of its own with content, or this long
    /// after its first frame of its own when all were black; 0 = at its first frame (the rule before 2026-11-15)
    pub ready_content_wait: u32,
    pub disks: Vec<DiskConfig>,
    pub display: DisplayBackend,
    pub display_format: DisplayFormat,
    pub gpu: Gpu,
    /// gpu-node=PATH: the amdgpu render node (gpu3d_amdgpu; "" = /dev/dri/renderD128)
    pub gpu_node: String,
    pub gpu_level: GpuLevel,
    /// gpu-priority=0: KGSL queue priorities as the guest asks, not by the owner's class (1)
    pub gpu_priority: bool,
    /// guest framebuffer; 0 = the panel's native (largest) mode, 1440x3120 where it cannot be read
    pub width: u32,
    pub height: u32,
    /// the guest display's refresh rate; 0 = the panel's highest at that mode, 120 where unknown
    pub hz: u32,
    /// panel-fallback=WxH[@HZ]: the mode assumed where the panel cannot be read (no helper report, no DRM panel; a
    /// bench run) instead of PANEL_*, for a phone whose panel is not the S24 Ultra's (the Galaxy S25 and A56:
    /// 1080x2340@120). None: unset; hz 0: not given.
    pub panel_fallback: Option<PanelMode>,
    /// UI scale, pixels per point: 3 (480x1040 points, the default) or 2 (720x1560)
    pub scale: u32,
    /// the QuartzCore text fixups a scale of 3 needs (scale_fixups): on by default
    pub scale_fixup: bool,
    /// corner-radius=N (0..200): /product display-corner-radius in points (0 = say nothing, the default).
    pub corner_radius: u32,
    /// artwork-subtype=N (0..10000): /product artwork-device-subtype (0 = from the panel: 2340 on 1080x2340, else 2868).
    pub artwork_subtype: u32,
    /// the iOS 16.5 (20F66) text fixups (ios16_fixups: CoreImage's OpenGL ES fallback answers no context): on by default
    pub ios16_fixup: bool,
    pub text_fixups: TextFixups,
    /// read /dev/input
    pub input: bool,
    /// a helper that can copies frames from guest RAM itself (display.rs); false: xnuhost copies (true)
    pub display_direct: bool,
    /// touch-exclusive=1: EVIOCGRAB the native sec_touchscreen only (never a key device); default off. Allowed in a
    /// bench+SHM preview (bridges=0; the side keys are then not read at all) and in session-mode=takeover, which turns
    /// it on (the side keys still read, never grabbed). Whether it was given: given("touch-exclusive") (session-mode=
    /// takeover then keeps that value)
    pub touch_exclusive: bool,
    /// session-mode=hosted (default: the session as before, byte for byte) or takeover (the opt-in Android takeover,
    /// docs/booter.md 2.9d: the host table must allow it, mode=takeover); given("session-mode"): the command line wins
    /// over the image set's
    pub session_mode: SessionMode,
    /// takeover-stops=default|none|NAME,... : the services of Android the OneOS switch stops in session-mode=takeover
    /// (docs/booter.md 2.9d, oneos/interface.md 4a). xnuhost only checks it (each name one of TAKEOVER_STOPPABLE) and
    /// names it in the takeover line; the switch reads it from the image's xnuhost.conf and does the stops. "" = not
    /// given (the switch's default list)
    pub takeover_stops: String,
    /// takeover-reclaim=0|1 (1) and takeover-swappiness=N|keep (keep; N 0-200): the OneOS switch's memory steps in
    /// session-mode=takeover, after its stops and before xnuhost (docs/booter.md 2.9d, "More RAM in the takeover";
    /// oneos/interface.md 4a): the caches dropped and zram compacted; vm.swappiness for the session, put back by the
    /// return. xnuhost only checks the words and names a value other than the default in the takeover line.
    pub takeover_reclaim: bool,
    pub takeover_swappiness: Option<u32>,
    /// --host-info: print the host table's answer for this phone (host-info: device=... takeover=...) and exit
    pub host_info: bool,
    /// touch-sleep=0: the brightness bridge never puts the touch IC to sleep (ONEIOS_TSP_OFF=0)
    pub touch_sleep: bool,
    /// a file of scripted touches and keys (input_script.rs), "" = none
    pub input_script: String,
    pub pen: Pen,
    pub pen_rotate: PenRotate,
    /// pen-mirror=1: then mirrored left to right
    pub pen_mirror: bool,
    pub sensors: Sensors,
    /// sensors-hz=N (5..200, default 50): the accelerometer's and gyroscope's rate (the magnetometer's at most 25)
    pub sensors_hz: u32,
    /// sensors-light=N: the hub's Nth ambient_light sensor; unset (auto): the first that does not say "rear"
    pub sensors_light: Option<u32>,
    pub encoder: Encoder,
    pub camera_vm: CameraVm,
    /// camera-model=NAME: /product unique-model with the cameras (MobileGestalt's HWModelDescriptionForCamera;
    /// CMCapture's per-model folder is NAME less "AP"). Default xnuloader's CAMERA_MODEL "OneiOSAP" (in no AVGestalt
    /// table: VirtualAudio starts; the kernel, hosted 0088, links CMCapture.framework/OneiOS to V54); V54AP: the
    /// iPhone's name, as from 2026-11-23 to the fix (AVGestalt answers V54's features, and VirtualAudio's webcam
    /// subport stops all audio). 3-15 letters and digits ending in "AP"
    pub camera_model: String,
    /// start the task factory
    pub tasks: bool,
    /// uid of iOS task processes; None = xnuhost's own
    pub task_uid: Option<u32>,
    /// log-level=N (0 errors, 1 info, 2 and more debug), -v debug
    pub log_level: Level,
    pub legacy_boot_args: bool,
    /// a trapped dc set/way ends the guest's set/way loop of its level at once (sysreg.rs): on by default
    pub setway_skip: bool,
    /// virtio_blk.rs: the Data store's flushes on a thread of their own, completed later (the guest driver asks): on
    pub blk_late_flush: bool,
    /// perf.rs: seconds between two "perf:" reports; 0 = only at the stop and on SIGQUIT; None = perf's default (30)
    pub perf_log: Option<u32>,
    /// task.rs: microseconds a vCPU (spin-host) and an iOS thread (spin-task) spin for the other side of a hand-off
    /// before they sleep in a futex; None = task's defaults
    pub spin_host: Option<u32>,
    pub spin_task: Option<u32>,
    pub cpu_pin: CpuPin,
    /// task.rs: task_map orders ride on the next release of one of the task's threads (on)
    pub map_defer: bool,
    /// The switches of docs/switches.md (each one key, read once at the start; the defaults are main's behaviour):
    /// wifi-ring-wait, wifi-send-wait, wifi-buffers (1), wifi-joined-scan (cache): the Wi-Fi bridge
    pub wifi_ring_wait: bool,
    pub wifi_send_wait: bool,
    pub wifi_buffers: bool,
    pub wifi_joined_scan: WifiJoinedScan,
    /// encoder-10bit (main10), encoder-warmup (1): the video encoder
    pub encoder_10bit: Encoder10,
    pub encoder_warmup: bool,
    /// astc (1): the device tree's GPU feature sets keep APPLE2 and MTL2,2, so MobileGestalt answers DeviceSupportsASTC
    /// and CoreUI and Core Animation hand ASTC images to the GPU (NativeMetal's frontend lays them out, DVMLinearTextureLayout)
    /// instead of decoding them on the CPU; 0: both dropped, as before 2026-11-25 (runtime/loader machine.rs set_featuresets)
    pub astc: bool,
    /// display-post (apart): the display helper reads and answers each frame while it waits for a buffer and posts
    /// the last (fix/frame-path); serial: it reads no frame until the last is posted (before 2026-11-24)
    pub display_post_serial: bool,
    /// display-present (ahb, 2026-11-25): the helper posts its frames in write-only AHardwareBuffers on a child of the
    /// layer (oneios-display xhd::present, ported from YumikoHayamato/OneiOS cdb297bbf: no CPU cache maintenance of the
    /// whole buffer at each lock and post); window: through the layer's window, as before
    pub display_present_ahb: bool,
    /// decoder (auto), decoder-codecs (h264,hevc,vp9,av1; bits of oneencoder::proto::DECODER_CODECS): the video decoder
    pub decoder: Decoder,
    pub decoder_codecs: u32,
    /// gpu-blob-entries=N (65532, 78c4b86; 16384 before): the most entries of a guest 3D blob the device takes
    pub gpu_blob_entries: u32,
    /// gpu-down (governor): when the GPU level leaves the top
    pub gpu_down: GpuDown,
    /// gpu-kgsl-ui-rb (0): keep render server and front app out of Android's UI ringbuffer (4-7)
    pub gpu_kgsl_ui_rb: bool,
    /// gpu-off-level (1): hold KGSL at its lowest level while iOS's display is off
    pub gpu_off_level: bool,
    /// stop-loop (wait): for the fork's stop loop; xnuhost checks it and names it in the switches line
    pub stop_loop: StopLoop,
    /// uclamp-rule (work): who sets the vCPU threads' clamps and where drm2kgsl's boost goes
    pub uclamp_rule: UclampRule,
    /// task.rs, "The trim": an idle task's host mappings are dropped (on)
    pub task_trim: bool,
    /// task-trim-idle=SECONDS (30): no release for this long and a task is trimmed
    pub task_trim_idle: u32,
    /// timer-coalesce (on): a later deadline or a cancel leaves the vCPU's host timer armed (cpu::TIMER_COALESCE)
    pub timer_coalesce: bool,
    /// thermal-cap= (on by default): at the cell's critical temperature, narrow the vCPUs to fewer host cores.
    /// Set to 0 to keep every core while still reporting the thermal level to iOS (NoNameBuilder/OneiOS 23cb7469).
    pub thermal_cap: bool,
    pub selftest: bool,
    /// runtime/bridges devices (network, sound, haptics, Bluetooth, CPU placement); by mode unless given
    pub bridges: bool,
    /// "EEPPPPPP": the cluster type of each vCPU, overriding the bridges' plan ("" = the plan, else all P)
    pub clusters: String,
    pub bluetooth: Bluetooth,
    pub haptics: Haptics,
    /// haptics-native-impacts: true the vibrator HAL's presets for iOS's taps, host and guest together (docs/hardware.md
    /// 5: hw-backend-haptics=treble, no fallback; oneios_native_impacts=1 added to the boot arguments); false off; None
    /// unset (ONEIOS_HAPTIC_NATIVE_IMPACTS decides, as before)
    pub haptics_native_impacts: Option<bool>,
    pub wifi: Wifi,
    /// hw-backend=samsung|treble, hw-backend-<bridge>=..., hw-backend-fallback=0|1: which form of the phone's hardware
    /// each bridge uses (onebridges::hw; docs/hardware.md, "Hardware backends")
    pub hw: HwSettings,
    /// The host phone (onebridges::hw; docs/hardware.md, "The host device"): host-device=auto|none|NAME ("" unset:
    /// auto), host-props=FILE (a getprop dump read instead of the property area), host-root=DIR (stands for / for the
    /// sysfs and proc fallbacks). The finish sets host (None: none or unknown) and host_line, the start's one line.
    pub host_device: String,
    pub host_props: String,
    pub host_root: String,
    pub host: Option<Host>,
    pub host_line: String,
}

impl Default for Config {
    fn default() -> Self {
        Config::new()
    }
}

/// C's copy(): a text that would not fit the C buffer of `size` bytes is refused.
fn text(value: &str, size: usize) -> Result<String, BadSetting> {
    if value.len() >= size {
        Err(BadSetting)
    } else {
        Ok(value.to_owned())
    }
}

/// What the OneOS switch may stop in session-mode=takeover (takeover-stops=): One UI's zygote's restart list, which
/// every return restarts already, less what a bridge uses (docs/plan-ios-first-session.md 4, stage 2b). The first six
/// are the switch's default; netd and media only when named (a Wi-Fi join and an encode without them are not yet
/// checked on the phone). The list is the switch catalog's (oneios-switches), which oneios-tool's `conf` checks with.
pub use oneios_switches::TAKEOVER_STOPPABLE;

/// takeover-stops=: default, none, or names of TAKEOVER_STOPPABLE separated by commas, each once.
fn takeover_stops_ok(value: &str) -> bool {
    if value == "default" || value == "none" {
        return true;
    }
    let names: Vec<&str> = value.split(',').collect();
    names.iter().all(|n| TAKEOVER_STOPPABLE.contains(n))
        && names.iter().enumerate().all(|(i, n)| !names[..i].contains(n))
}

/// camera-model=: 3 to 15 letters and digits that end in "AP" (CMCapture takes the name less "AP" as a folder name;
/// the kernel, hosted 0088, makes a link of that name), so nothing else reaches the device tree or a path.
fn camera_model_ok(value: &str) -> bool {
    (3..=15).contains(&value.len()) && value.ends_with("AP") && value.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// One of a setting's names.
fn named<T>(parsed: Option<T>) -> Result<T, BadSetting> {
    parsed.ok_or(BadSetting)
}

/// 1/yes/true, 0/no/false.
fn flag(value: &str) -> Result<bool, BadSetting> {
    match value {
        "1" | "yes" | "true" => Ok(true),
        "0" | "no" | "false" => Ok(false),
        _ => Err(BadSetting),
    }
}

/// A whole decimal number in [lo, hi] (strtol: leading blanks and a sign allowed, nothing after).
fn number(value: &str, lo: i64, hi: i64) -> Result<u32, BadSetting> {
    match parse::strtol(value, 10) {
        Some((v, rest)) if rest.is_empty() && (lo..=hi).contains(&v) => Ok(v as u32),
        _ => Err(BadSetting),
    }
}

/// A size below 1 TiB.
fn size_below_tib(value: &str) -> Result<u64, BadSetting> {
    match parse_size(value) {
        Some(v) if v < 1 << 40 => Ok(v),
        _ => Err(BadSetting),
    }
}

fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// `%Nu` of sscanf: optional blanks, an optional '+', 1..=width digits. The value and what follows.
fn scan_unsigned(text: &str, width: usize) -> Option<(u32, &str)> {
    let t = text.trim_start_matches(|c: char| c.is_ascii() && is_c_space(c as u8));
    let (t, budget) = match t.strip_prefix('+') {
        Some(rest) => (rest, width - 1),
        None => (t, width),
    };
    let n = t.bytes().take(budget).take_while(u8::is_ascii_digit).count();
    if n == 0 {
        return None;
    }
    Some((t[..n].parse().ok()?, &t[n..]))
}

/// panel-fallback's WxH or WxH@HZ.
fn panel_fallback(value: &str) -> Option<PanelMode> {
    let (width, rest) = scan_unsigned(value, 5)?;
    let rest = rest.strip_prefix('x')?;
    let (height, rest) = scan_unsigned(rest, 5)?;
    let hz = match rest.strip_prefix('@') {
        None if rest.is_empty() => 0,
        None => return None,
        Some(rest) => match scan_unsigned(rest, 3)? {
            (hz, "") if (24..=240).contains(&hz) => hz,
            _ => return None,
        },
    };
    (width > 0 && height > 0 && width <= 16384 && height <= 16384).then_some(PanelMode { width, height, hz })
}

impl Config {
    /// xh_config_defaults.
    pub fn new() -> Self {
        // One vCPU per core of the host, up to the 8 the phone has (the bridges give them its cluster types).
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        Config {
            given: BTreeSet::new(),
            unsupported_mode: None,
            unsupported_display: None,
            session_abi: 1,
            mode: Mode::Takeover,
            runtime: String::new(),
            image: String::new(),
            data: String::new(),
            session: String::new(),
            kernel: String::new(),
            identity: String::new(),
            boot_args: String::new(),
            boot_args_add: String::new(),
            ncpu: cores.clamp(1, MAX_CPUS) as u32,
            ram_bytes: 0, // ram=auto
            ram_view: 0,
            ram_percent: RAM_AUTO_PERCENT,
            ram_host_floor: RAM_AUTO_FLOOR,
            ram_meminfo: String::new(),
            ram_auto_total: 0,
            ram_auto_available: 0,
            ram_auto_bound: None,
            ram_giveback: false,
            ram_ondemand: false,
            ram_prefault: true,
            ram_lock: true,
            ram_lock_expected: false,
            ram_session_reserve: RAM_SESSION_RESERVE,
            ram_unlock_below: RAM_UNLOCK_BELOW,
            ram_fit_on: true,
            ram_huge: true,
            ram_gpu_reserve: RAM_GPU_RESERVE,
            ram_pagetable_percent: RAM_PAGETABLE_PERCENT,
            ram_hold_percent: RAM_HOLD_PERCENT,
            ram_tasks: RAM_TASKS,
            ram_task_cost: RAM_TASK_COST,
            ram_fit_terms_old: false,
            ram_tasks_awake: RAM_TASKS_AWAKE,
            ram_task_idle_cost: RAM_TASK_IDLE_COST,
            ram_fit: 0,
            ram_fit_available: 0,
            ram_lock_fit: 0,
            host_mem_log: 30,
            usb_log: 3,
            return_graceful_ms: 0,
            ready_content_wait: READY_CONTENT_WAIT,
            disks: Vec::new(),
            display: DisplayBackend::Shm,
            display_format: DisplayFormat::Bgrx8,
            gpu: Gpu::Auto,
            gpu_level: GpuLevel::Deadline,
            gpu_node: String::new(),
            gpu_priority: true,
            width: 0,
            height: 0,
            hz: 0,
            panel_fallback: None,
            scale: 3,
            scale_fixup: true,
            corner_radius: 0,
            artwork_subtype: 0,
            ios16_fixup: true,
            text_fixups: TextFixups::default(),
            input: true,
            display_direct: true,
            touch_exclusive: false, // the touchscreen grab: a bench preview, or session-mode=takeover (which turns it on)
            session_mode: SessionMode::Hosted, // the default session, byte for byte as before session-mode existed
            takeover_stops: String::new(),
            takeover_reclaim: true,
            takeover_swappiness: None,
            host_info: false,
            touch_sleep: true,
            input_script: String::new(),
            pen: Pen::Off,
            pen_rotate: PenRotate::Auto,
            pen_mirror: false,
            sensors: Sensors::Auto, // on by default since 2026-11-25 (the owner: every new fix on); off: no socket opened
            sensors_hz: 50,
            sensors_light: None,
            encoder: Encoder::Off,
            camera_vm: CameraVm::Hide,
            camera_model: CAMERA_MODEL.to_owned(),
            tasks: true,
            task_uid: None,
            log_level: Level::Info,
            legacy_boot_args: false,
            setway_skip: true,
            blk_late_flush: true,
            perf_log: None,
            spin_host: None,
            spin_task: None,
            cpu_pin: CpuPin::On,
            map_defer: true,
            wifi_ring_wait: true,
            wifi_send_wait: true,
            wifi_buffers: true,
            wifi_joined_scan: WifiJoinedScan::Cache,
            encoder_10bit: Encoder10::Main10,
            encoder_warmup: true,
            astc: true,
            display_post_serial: false,
            display_present_ahb: true,
            decoder: Decoder::Auto,
            decoder_codecs: oneencoder::switches::ALL_CODECS,
            gpu_blob_entries: GPU_BLOB_ENTRIES,
            gpu_down: GpuDown::Governor,
            gpu_kgsl_ui_rb: false,
            gpu_off_level: true,
            stop_loop: StopLoop::Wait,
            uclamp_rule: UclampRule::Work,
            task_trim: true,
            task_trim_idle: TASK_TRIM_IDLE,
            timer_coalesce: true,
            thermal_cap: true,
            selftest: false,
            bridges: true,
            clusters: String::new(),
            bluetooth: Bluetooth::Auto,
            haptics: Haptics::Auto,
            haptics_native_impacts: None,
            wifi: Wifi::Auto,
            hw: HwSettings::default(), // every bridge samsung unless hw-backend says otherwise
            host_device: String::new(),
            host_props: String::new(),
            host_root: String::new(),
            host: None,
            host_line: String::new(),
        }
    }

    /// Whether `key` was set here (the command line, or this file): what C kept as "not -1".
    pub fn given(&self, key: &str) -> bool {
        self.given.contains(key)
    }

    /// xh_config_set: one KEY=VALUE. A refused value changes nothing.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), BadSetting> {
        let mut next = self.clone();
        next.apply(key, value)?;
        if let Some(k) = GIVEN_KEYS.iter().find(|k| **k == key) {
            next.given.insert(k);
        }
        *self = next;
        Ok(())
    }

    fn apply(&mut self, key: &str, value: &str) -> Result<(), BadSetting> {
        let fmt_ok = |ok: bool| if ok { Ok(()) } else { Err(BadSetting) };
        match key {
            "session-abi" => self.session_abi = parse::atoi(value),
            "mode" => {
                text(value, 16)?;
                match Mode::parse(value) {
                    Some(m) => {
                        self.mode = m;
                        self.unsupported_mode = None;
                    }
                    None => self.unsupported_mode = Some(value.to_owned()),
                }
            }
            "runtime" => self.runtime = text(value, 512)?,
            "image" => self.image = text(value, 512)?,
            "data" => self.data = text(value, 512)?,
            "session" => self.session = text(value, 512)?,
            "kernel" => self.kernel = text(value, 512)?,
            "identity" => self.identity = text(value, 512)?,
            "boot-args" => self.boot_args = text(value, BOOT_ARGS_SIZE)?,
            "boot-args-add" => self.boot_args_add = text(value, 512)?,
            "display" => {
                text(value, 16)?;
                match DisplayBackend::parse(value) {
                    Some(d) => {
                        self.display = d;
                        self.unsupported_display = None;
                    }
                    None => self.unsupported_display = Some(value.to_owned()),
                }
            }
            "display-format" => self.display_format = named(DisplayFormat::parse(value))?,
            "gpu" => self.gpu = named(Gpu::parse(value))?,
            "gpu-level" => self.gpu_level = named(GpuLevel::parse(value))?,
            "gpu-node" => self.gpu_node = text(value, 128)?,
            "gpu-priority" => self.gpu_priority = number(value, 0, 1)? == 1,
            "cpus" => {
                let n = i64::from(parse::atoi(value));
                fmt_ok((1..=MAX_CPUS as i64).contains(&n))?;
                self.ncpu = n as u32;
            }
            "ram" => {
                self.ram_bytes = if value == "auto" {
                    0
                } else {
                    match parse_size(value) {
                        Some(v) if v != 0 => v,
                        _ => return Err(BadSetting),
                    }
                };
            }
            "ram-view" => {
                self.ram_view = match value {
                    "off" => 0,
                    "auto" => RAM_VIEW_AUTO,
                    _ => size_below_tib(value)?,
                }
            }
            "ram-percent" => self.ram_percent = number(value, 10, 100)?,
            "ram-host-floor" => self.ram_host_floor = size_below_tib(value)?,
            "ram-meminfo" => self.ram_meminfo = text(value, 256)?,
            "host-mem-log" => self.host_mem_log = number(value, 0, 86400)?,
            "usb-log" => self.usb_log = number(value, 0, 3600)?,
            "ram-giveback" => self.ram_giveback = flag(value)?,
            "ram-ondemand" => self.ram_ondemand = flag(value)?,
            "ram-prefault" => self.ram_prefault = flag(value)?,
            "ram-lock" => self.ram_lock = flag(value)?,
            "ram-session-reserve" => self.ram_session_reserve = size_below_tib(value)?,
            "ram-fit" => self.ram_fit_on = flag(value)?,
            "ram-huge" => self.ram_huge = flag(value)?,
            "ram-gpu-reserve" => self.ram_gpu_reserve = size_below_tib(value)?,
            "ram-pagetable-percent" => self.ram_pagetable_percent = number(value, 0, 50)?,
            "ram-hold-percent" => self.ram_hold_percent = number(value, 0, 50)?,
            "ram-tasks" => self.ram_tasks = number(value, 0, 4096)?,
            "ram-task-cost" => self.ram_task_cost = size_below_tib(value)?,
            "ram-fit-terms" => {
                self.ram_fit_terms_old = match value {
                    "tasks" => false,
                    "old" => true,
                    _ => return Err(BadSetting),
                }
            }
            "ram-unlock-below" => self.ram_unlock_below = size_below_tib(value)?,
            "return-graceful-ms" => self.return_graceful_ms = number(value, 0, 120000)?,
            "ready-content-wait" => self.ready_content_wait = number(value, 0, 480)?,
            "width" => self.width = parse::atoi(value) as u32,
            "height" => self.height = parse::atoi(value) as u32,
            "hz" => {
                let hz = parse::atoi(value) as u32;
                fmt_ok(hz == 0 || (24..=240).contains(&hz))?;
                self.hz = hz;
            }
            // WxH or WxH@HZ: the mode a session assumes when the panel cannot be read (panel_fallback)
            "panel-fallback" => self.panel_fallback = Some(panel_fallback(value).ok_or(BadSetting)?),
            "scale" => {
                let scale = parse::atoi(value) as u32;
                fmt_ok(scale == 2 || scale == 3)?;
                self.scale = scale;
            }
            "scale-fixup" => self.scale_fixup = flag(value)?,
            "corner-radius" => {
                let radius = parse::atoi(value) as u32;
                fmt_ok(radius <= 200)?;
                self.corner_radius = radius;
            }
            "artwork-subtype" => {
                let subtype = parse::atoi(value) as u32;
                fmt_ok(subtype <= 10000)?;
                self.artwork_subtype = subtype;
            }
            "ios16-fixup" => self.ios16_fixup = flag(value)?,
            "text-fixup" => {
                // FILE,OFFSET,ORIGINAL,REPLACEMENT in hex, as UniXNU's [payload] text_fixups (the kernel checks them again)
                let commas = value.bytes().filter(|&b| b == b',').count();
                fmt_ok(commas == 3 && value.len() < 160)?;
                fmt_ok(self.text_fixups.push_within(value, TEXT_FIXUPS_SIZE))?;
            }
            "input" => self.input = flag(value)?,
            "display-direct" => self.display_direct = flag(value)?,
            "touch-exclusive" => self.touch_exclusive = flag(value)?,
            // hosted (the default) or takeover (docs/booter.md 2.9d): the finish checks the host table and the mode
            "session-mode" => self.session_mode = named(SessionMode::parse(value))?,
            // the switch's stop list in session-mode=takeover: checked here, done by the switch
            "takeover-stops" if takeover_stops_ok(value) => self.takeover_stops = value.to_owned(),
            "takeover-stops" => return Err(BadSetting),
            "takeover-reclaim" => self.takeover_reclaim = flag(value)?,
            "takeover-swappiness" if value == "keep" => self.takeover_swappiness = None,
            "takeover-swappiness" => self.takeover_swappiness = Some(number(value, 0, 200)?),
            "input-script" => self.input_script = text(value, 512)?,
            "tasks" => self.tasks = flag(value)?,
            "task-uid" => self.task_uid = u32::try_from(parse::atoi(value)).ok(),
            "touch-sleep" => self.touch_sleep = flag(value)?,
            "pen" => self.pen = named(Pen::parse(value))?,
            "pen-rotate" => self.pen_rotate = named(PenRotate::parse(value))?,
            "pen-mirror" => self.pen_mirror = flag(value)?,
            "sensors" => self.sensors = named(Sensors::parse(value))?,
            "sensors-hz" => self.sensors_hz = number(value, 5, 200)?,
            "sensors-light" => {
                self.sensors_light = if value == "auto" { None } else { Some(number(value, 0, 7)?) };
            }
            "encoder" => self.encoder = named(Encoder::parse(value))?,
            "camera-vm" => self.camera_vm = named(CameraVm::parse(value))?,
            "camera-model" => {
                fmt_ok(camera_model_ok(value))?;
                self.camera_model = value.to_owned();
            }
            "bridges" => self.bridges = flag(value)?,
            "clusters" => {
                fmt_ok(value.len() <= MAX_CPUS && value.bytes().all(|b| b == b'E' || b == b'P'))?;
                self.clusters = value.to_owned();
            }
            "wifi" => self.wifi = named(Wifi::parse(value))?,
            "bluetooth" => self.bluetooth = named(Bluetooth::parse(value))?,
            "haptics" => self.haptics = named(Haptics::parse(value))?,
            "haptics-native-impacts" => self.haptics_native_impacts = Some(flag(value)?),
            "legacy-boot-args" => self.legacy_boot_args = flag(value)?,
            "setway-skip" => self.setway_skip = flag(value)?,
            "blk-late-flush" => self.blk_late_flush = flag(value)?,
            "perf-log" => self.perf_log = Some(number(value, 0, 86400)?),
            "spin-host" => self.spin_host = Some(number(value, 0, 1000)?),
            "spin-task" => self.spin_task = Some(number(value, 0, 1000)?),
            "cpu-pin" => {
                self.cpu_pin = match value {
                    "2" => CpuPin::Always,
                    _ if flag(value)? => CpuPin::On,
                    _ => CpuPin::Off,
                }
            }
            "map-defer" => self.map_defer = flag(value)?,
            "wifi-ring-wait" => self.wifi_ring_wait = flag(value)?,
            "wifi-send-wait" => self.wifi_send_wait = flag(value)?,
            "wifi-buffers" => self.wifi_buffers = flag(value)?,
            "wifi-joined-scan" => self.wifi_joined_scan = named(WifiJoinedScan::parse(value))?,
            "encoder-10bit" => self.encoder_10bit = named(Encoder10::parse(value))?,
            "encoder-warmup" => self.encoder_warmup = flag(value)?,
            "astc" => self.astc = flag(value)?,
            "display-post" => {
                self.display_post_serial = named(match value {
                    "apart" => Some(false),
                    "serial" => Some(true),
                    _ => None,
                })?
            }
            "display-present" => {
                self.display_present_ahb = named(match value {
                    "ahb" => Some(true),
                    "window" => Some(false),
                    _ => None,
                })?
            }
            "decoder" => self.decoder = named(Decoder::parse(value))?,
            "decoder-codecs" => self.decoder_codecs = named(oneencoder::switches::parse_codecs(value))?,
            "gpu-blob-entries" => self.gpu_blob_entries = number(value, 16384, i64::from(GPU_BLOB_ENTRIES))?,
            "gpu-down" => self.gpu_down = named(GpuDown::parse(value))?,
            "gpu-kgsl-ui-rb" => self.gpu_kgsl_ui_rb = flag(value)?,
            "gpu-off-level" => self.gpu_off_level = flag(value)?,
            "stop-loop" => self.stop_loop = named(StopLoop::parse(value))?,
            "uclamp-rule" => self.uclamp_rule = named(UclampRule::parse(value))?,
            "task-trim" => self.task_trim = flag(value)?,
            "task-trim-idle" => self.task_trim_idle = number(value, 1, 86400)?,
            "timer-coalesce" => self.timer_coalesce = flag(value)?,
            "thermal-cap" => self.thermal_cap = flag(value)?,
            "ram-tasks-awake" => self.ram_tasks_awake = number(value, 0, 4096)?,
            "ram-task-idle-cost" => self.ram_task_idle_cost = size_below_tib(value)?,
            // hw-backend, hw-backend-<bridge>, hw-backend-fallback (onebridges::hw)
            _ if key.starts_with("hw-backend") => self.hw.setting(key, value).map_err(|_| BadSetting)?,
            "host-device" => {
                // auto, none or a name of the host table
                let known = value.eq_ignore_ascii_case("auto")
                    || value.eq_ignore_ascii_case("none")
                    || bhw::host_find(value).is_some();
                fmt_ok(known)?;
                self.host_device = text(value, 32)?;
            }
            "host-props" => self.host_props = text(value, 256)?,
            "host-root" => self.host_root = text(value, 256)?,
            "log-level" => self.log_level = Level::from_index(parse::atoi(value).into()),
            "disk" | "disk-rw" | "data-store" => {
                // disk=FILE (read-only), disk-rw=FILE[,SEED], data-store=FILE[,SEED]
                fmt_ok(self.disks.len() < MAX_DISKS)?;
                let (path, seed) = match value.split_once(',') {
                    Some((path, seed)) => (path, Some(seed)),
                    None => (value, None),
                };
                let writable = key != "disk";
                fmt_ok(!path.is_empty() && path.len() < 512 && (seed.is_none() || writable))?;
                let seed = match seed {
                    Some(seed) => text(seed, 512)?,
                    None => String::new(),
                };
                self.disks.push(DiskConfig { path: path.to_owned(), writable, data_store: key == "data-store", seed });
            }
            _ => return Err(BadSetting),
        }
        Ok(())
    }

    /// xh_config_file: the "key=value" lines of a file (a missing file is fine; blank lines and # comments skipped).
    pub fn file(&mut self, path: &str) -> Result<(), ConfigError> {
        let file = match fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(ConfigError::silent()),
        };
        for (index, line) in BufReader::new(file).split(b'\n').enumerate() {
            let line = line.map_err(|_| ConfigError::silent())?;
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_matches(|c: char| c.is_ascii() && is_c_space(c as u8));
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or_else(ConfigError::silent)?;
            if self.set(key, value).is_err() {
                return Err(ConfigError::new(format!("{path}:{}: bad setting {key}", index + 1)));
            }
        }
        Ok(())
    }

    /// xh_config_args: the ONEIOS_* environment, then argv (argv[0] is the program).
    pub fn args(&mut self, argv: &[String]) -> Result<(), ConfigError> {
        self.args_with_env(argv, |name| std::env::var(name).ok())
    }

    /// [`Config::args`] with the environment read through `env` (the tests).
    pub fn args_with_env(&mut self, argv: &[String], env: impl Fn(&str) -> Option<String>) -> Result<(), ConfigError> {
        const ENVS: &[(&str, &str)] = &[
            ("ONEIOS_SESSION_ABI", "session-abi"),
            ("ONEIOS_MODE", "mode"),
            ("ONEIOS_RUNTIME", "runtime"),
            ("ONEIOS_IMAGE", "image"),
            ("ONEIOS_DATA", "data"),
            ("ONEIOS_SESSION", "session"),
        ];
        for (name, key) in ENVS {
            if let Some(v) = env(name).filter(|v| !v.is_empty()) {
                if self.set(key, &v).is_err() {
                    return Err(ConfigError::new(format!("bad {name}")));
                }
            }
        }
        let mut i = 1;
        while i < argv.len() {
            let a = argv[i].as_str();
            i += 1;
            match a {
                "-v" => self.log_level = Level::Debug,
                "--selftest" => self.selftest = true,
                "--host-info" => self.host_info = true,
                "--no-input" => self.input = false,
                "--no-tasks" => self.tasks = false,
                "-h" | "--help" => return Err(ConfigError { lines: Vec::new(), usage: true }),
                _ if !a.starts_with("--") || i >= argv.len() => {
                    return Err(ConfigError::new(format!("unknown or incomplete argument {a}")).with_usage());
                }
                _ => {
                    let v = argv[i].as_str();
                    i += 1;
                    if a == "--config" {
                        if let Err(mut e) = self.file(v) {
                            e.lines.push(format!("cannot use {v}"));
                            return Err(e);
                        }
                    } else if a == "--set" {
                        let Some((key, value)) = v.split_once('=').filter(|_| v.len() < 1200) else {
                            return Err(ConfigError::new("--set takes KEY=VALUE"));
                        };
                        if self.set(key, value).is_err() {
                            return Err(ConfigError::new(format!("bad setting {key}")));
                        }
                    } else if self.set(&a[2..], v).is_err() {
                        return Err(ConfigError::new(format!("bad value for {a}")));
                    }
                }
            }
        }
        Ok(())
    }

    /// xh_config_finish: merges the image set's settings and the host phone's defaults, fills what depends on other
    /// values. `can_lock`: whether a lock of the RAM can work here (a test's answer, or ramfix::can_lock).
    pub fn finish_with(&mut self, can_lock: &dyn Fn() -> bool) -> Result<(), ConfigError> {
        if self.session_abi != 1 {
            return Err(ConfigError::new(format!("session ABI {} is not supported (this build: 1)", self.session_abi)));
        }
        if let Some(mode) = &self.unsupported_mode {
            return Err(ConfigError::new(format!("mode {mode} is not supported")));
        }
        if let Some(display) = &self.unsupported_display {
            return Err(ConfigError::new(format!("display {display} is not supported")));
        }
        // The image set may carry settings of its own; the command line was applied first and a setting there wins for
        // single values (disks add up). Not read for a selftest, which needs no image.
        let mut image = Config::new();
        if !self.selftest {
            if self.image.is_empty() {
                return Err(ConfigError::new("--image DIR is required"));
            }
            image.file(&format!("{}/xnuhost.conf", self.image))?;
            // The userland hook's settings of this image (the UI scale its Data seed was made for), under the same rule.
            image.file(&format!("{}/overlay/userland/xnuhost.conf", self.image))?;
        }
        // The host bridges change the phone's state (a tap, routing rules, the motor): on in a session. bridges=,
        // wifi=, bluetooth= and haptics= of the image set apply when the command line has none (until 2026-11-21 they
        // were read and then ignored, though docs/booter.md and main.c's Bluetooth line said otherwise).
        if !self.given("bridges") {
            self.bridges = if image.given("bridges") { image.bridges } else { self.mode == Mode::Takeover };
        }
        take!(self, image, "wifi", wifi);
        take!(self, image, "bluetooth", bluetooth);
        take!(self, image, "haptics", haptics);
        // pen=, pen-rotate=, pen-mirror= of the image set (its overlay/userland/xnuhost.conf on the phone). Until
        // 2026-11-22 they were read and then dropped: the phone's pen=auto never turned the pen on (no `pen:` line in
        // any session).
        take!(self, image, "pen", pen);
        take!(self, image, "pen-rotate", pen_rotate);
        take!(self, image, "pen-mirror", pen_mirror);
        // sensors=, sensors-hz=, sensors-light= of the image set: how a phone session turns the sensor hub off (sensors=off) or to ssc.
        take!(self, image, "sensors", sensors);
        take!(self, image, "sensors-hz", sensors_hz);
        take!(self, image, "sensors-light", sensors_light);
        // encoder= of the image set: the way a phone session turns the video encoder on (the OneOS app passes no --set
        // encoder=). Until 2026-11-22 it was read and then dropped, so encoder=auto in xnuhost.conf did nothing.
        take!(self, image, "encoder", encoder);
        take!(self, image, "camera-vm", camera_vm);
        take!(self, image, "camera-model", camera_model);
        take!(self, image, "takeover-stops", takeover_stops);
        take!(self, image, "takeover-reclaim", takeover_reclaim);
        take!(self, image, "takeover-swappiness", takeover_swappiness);
        // display= of the image set (image/xnuhost.conf or overlay/userland/xnuhost.conf): until 2026-11-27 it was
        // parsed into `image` and dropped, so display=drm in takeover left xnuhost on shm while the switch stopped
        // surfaceflinger and skipped oneios-display (black panel). The command line still wins.
        if !self.given("display") && image.given("display") {
            if let Some(display) = &image.unsupported_display {
                return Err(ConfigError::new(format!("display {display} is not supported")));
            }
            self.display = image.display;
        }
        take!(self, image, "touch-exclusive", touch_exclusive);
        // session-mode= of the image set (its overlay/userland/xnuhost.conf) when the command line has none.
        take!(self, image, "session-mode", session_mode);
        let takeover_mode = self.session_mode == SessionMode::Takeover;
        // The touchscreen grab, outside the opt-in Android takeover, is a bounded non-takeover diagnostic only: never let
        // an input-grabbing guest take control of Android outside a SHM bench.
        if self.touch_exclusive
            && !takeover_mode
            && (self.mode != Mode::Bench || self.display != DisplayBackend::Shm || !self.input || self.bridges)
        {
            return Err(ConfigError::new(
                "touch-exclusive=1 requires session-mode=takeover, or mode=bench, display=shm, input=1, bridges=0",
            ));
        }
        if self.selftest {
            return Ok(());
        }
        // The host phone's defaults (onebridges::hw), under both: the command line > the image set > the table > built in.
        let device = self.host_device_layer(&image)?;
        // session-mode=takeover (docs/booter.md 2.9d): only in the OneOS session, only where the host table allows it (the
        // switch asked --host-info before it stopped One UI; this is the same rule, for a command line that did not), and
        // with input: it turns the native touchscreen's grab on unless touch-exclusive= was given.
        if takeover_mode {
            if self.mode != Mode::Takeover {
                return Err(ConfigError::new("session-mode=takeover needs mode=takeover (the OneOS session)"));
            }
            match self.host {
                Some(h) if h.0.takeover => {}
                _ => {
                    return Err(ConfigError::new(format!(
                        "session-mode=takeover is not supported on this phone ({}): the host table \
                         (runtime/bridges/hw/host_device.rs) does not allow it; session-mode=hosted is the session as before",
                        self.host.map_or("no row of the host table", |h| h.0.name)
                    )))
                }
            }
            if !self.input {
                return Err(ConfigError::new("session-mode=takeover needs input=1 (its touchscreen grab)"));
            }
            if !self.given("touch-exclusive") && !image.given("touch-exclusive") {
                self.touch_exclusive = true;
            }
        }
        if !self.given("gpu") {
            self.gpu = if image.given("gpu") { image.gpu } else { device.gpu };
        }
        if self.gpu_node.is_empty() {
            self.gpu_node = image.gpu_node.clone();
        }
        if self.boot_args.is_empty() {
            self.boot_args = image.boot_args.clone();
        }
        if self.width == 0 {
            self.width = image.width;
        }
        take!(self, image, "setway-skip", setway_skip);
        take!(self, image, "blk-late-flush", blk_late_flush);
        take!(self, image, "perf-log", perf_log);
        take!(self, image, "spin-host", spin_host);
        take!(self, image, "gpu-level", gpu_level);
        take!(self, image, "gpu-priority", gpu_priority);
        take!(self, image, "spin-task", spin_task);
        take!(self, image, "map-defer", map_defer);
        take!(self, image, "wifi-ring-wait", wifi_ring_wait);
        take!(self, image, "wifi-send-wait", wifi_send_wait);
        take!(self, image, "wifi-buffers", wifi_buffers);
        take!(self, image, "wifi-joined-scan", wifi_joined_scan);
        take!(self, image, "encoder-10bit", encoder_10bit);
        take!(self, image, "encoder-warmup", encoder_warmup);
        take!(self, image, "display-post", display_post_serial);
        take!(self, image, "display-present", display_present_ahb);
        take!(self, image, "decoder", decoder);
        take!(self, image, "decoder-codecs", decoder_codecs);
        take!(self, image, "astc", astc);
        take!(self, image, "gpu-blob-entries", gpu_blob_entries);
        take!(self, image, "gpu-down", gpu_down);
        take!(self, image, "gpu-kgsl-ui-rb", gpu_kgsl_ui_rb);
        take!(self, image, "gpu-off-level", gpu_off_level);
        take!(self, image, "stop-loop", stop_loop);
        take!(self, image, "uclamp-rule", uclamp_rule);
        take!(self, image, "task-trim", task_trim);
        take!(self, image, "task-trim-idle", task_trim_idle);
        take!(self, image, "timer-coalesce", timer_coalesce);
        take!(self, image, "thermal-cap", thermal_cap);
        if !self.given("thermal-cap") && !image.given("thermal-cap") && device.given("thermal-cap") {
            self.thermal_cap = device.thermal_cap;
        }
        take!(self, image, "cpu-pin", cpu_pin);
        self.hw.merge(&image.hw); // each hw-backend setting: the command line's, else the image set's,
        self.hw.merge(&device.hw); // else the host phone's
                                   // haptics-native-impacts: the command line's, else the image set's, else unset (None: ONEIOS_HAPTIC_NATIVE_IMPACTS
                                   // decides, as before). On only with the vibrator HAL and no fallback to evdev: the presets are the HAL's.
        take!(self, image, "haptics-native-impacts", haptics_native_impacts);
        if self.haptics_native_impacts == Some(true)
            && (!self.bridges
                || self.haptics != Haptics::Auto
                || self.hw.choose(Bridge::Haptics, Backend::Unset).0 != Backend::Treble
                || self.hw.fallback_allowed())
        {
            return Err(ConfigError::new(
                "haptics-native-impacts=1 needs bridges=1, haptics=auto, hw-backend-haptics=treble and hw-backend-fallback=0",
            ));
        }
        if self.height == 0 {
            self.height = image.height;
        }
        if self.hz == 0 {
            self.hz = image.hz;
        }
        if self.hz == 0 {
            self.hz = device.hz; // the host phone's rate (the galaxy-a56 row: 60, the fork's CPU-composited A56)
        }
        // panel-fallback: the command line's, else the image set's, else the host phone's
        self.panel_fallback = self.panel_fallback.or(image.panel_fallback).or(device.panel_fallback);
        take!(self, image, "display-direct", display_direct);
        take!(self, image, "display-format", display_format);
        // The KMS back end scans out 32-bit pixels: half floats only go to a display helper (shm) or nowhere.
        if self.display_format == DisplayFormat::Rgba16f && self.display == DisplayBackend::Drm {
            return Err(ConfigError::new("display-format=rgba16f needs --display shm"));
        }
        take!(self, image, "scale", scale);
        // The host phone's guest frame (width, height and its UI scale, taken as one): only where neither the command line
        // nor the image set gives a width or a height. Its scale then wins over the image set's, which is the scale of the
        // panel-sized frame roottweak made the image for, but not over the command line's. The galaxy-a56 row: 720x1560 @2x,
        // the fork's A56 frame since 2026-11-22 (Musicalista/OneiOS-A56 c83b9ca: the same 360x780 points as 1080x2340 @3x,
        // 2.25x fewer pixels for the CPU compositor; the display helper scales it to the panel).
        // return-graceful-ms: the command line's, else the image set's (finish_ram), else the host phone's (galaxy-a56: 4000,
        // in which kernel 0106 sends logd SIGTERM so iOS's log reaches the Data store).
        if !self.given("return-graceful-ms") && !image.given("return-graceful-ms") && device.given("return-graceful-ms")
        {
            self.return_graceful_ms = device.return_graceful_ms;
        }
        // astc: the command line's, else the image set's (the take below), else the host phone's (galaxy-a56: 0, its GPU
        // is not the Adreno this was measured on).
        if !self.given("astc") && !image.given("astc") && device.given("astc") {
            self.astc = device.astc;
        }
        // pen= and encoder=: the command line's, else the image set's (the takes above), else the host phone's (the S24
        // Ultra: auto, 2026-11-28). Until then both came only from the image set's xnuhost.conf, and an image rebuilt
        // without those lines (iOS 27.2.1 24B156, sessions of 2026-11-27) had no S Pen (no `pen:` line) and no hardware
        // video decoder (`ONEIOS CODEC: 0 virtio-media device(s)`): Safari's video was decoded in software on the CPU.
        if !self.given("pen") && !image.given("pen") && device.given("pen") {
            self.pen = device.pen;
        }
        if !self.given("encoder") && !image.given("encoder") && device.given("encoder") {
            self.encoder = device.encoder;
        }
        if self.width == 0 && self.height == 0 && device.width != 0 && device.height != 0 {
            self.width = device.width;
            self.height = device.height;
            if device.given("scale") && !self.given("scale") {
                self.scale = device.scale;
            }
        }
        take!(self, image, "scale-fixup", scale_fixup);
        take!(self, image, "corner-radius", corner_radius);
        if !self.given("corner-radius") && !image.given("corner-radius") && device.given("corner-radius") {
            self.corner_radius = device.corner_radius;
        }
        take!(self, image, "artwork-subtype", artwork_subtype);
        if !self.given("artwork-subtype") && !image.given("artwork-subtype") && device.given("artwork-subtype") {
            self.artwork_subtype = device.artwork_subtype;
        }
        take!(self, image, "ios16-fixup", ios16_fixup);
        if self.text_fixups.is_empty() {
            self.text_fixups = image.text_fixups.clone();
        }
        // Neither the command line nor the image set gave any: what the hosted kernel needs to start on this machine
        // (the session's command line has no boot arguments). From Droid-VM's ipados27/board.toml kernel_args, the
        // ones about the machine: SMP and ArmCpu's IPIs, no contiguous-hint mappings (the collection's layout:
        // arm_vm_init.c:1107 asserts without it), no interrupt-masked or preemption watchdogs (a vCPU is a host
        // thread), the source storage, the GPU's 3D context, the console in the log.
        if self.boot_args.is_empty() {
            self.boot_args = DEFAULT_BOOT_ARGS.to_owned();
        }
        // The host table's boot-args-add comes first (e.g. galaxy-s25's built-in side keys), then the command line's
        // or image set's; a word boot_args names already is not added again (NoNameBuilder/OneiOS 23b9b85f).
        if !device.boot_args_add.is_empty()
            && add_boot_args(&mut self.boot_args, BOOT_ARGS_SIZE, &device.boot_args_add).is_err()
        {
            return Err(ConfigError::new(format!(
                "device boot-args-add does not fit beside the guest boot args ({} bytes at most)",
                BOOT_ARGS_SIZE - 1
            )));
        }
        if self.boot_args_add.is_empty() {
            self.boot_args_add = image.boot_args_add.clone();
        }
        let add = self.boot_args_add.clone();
        if add_boot_args(&mut self.boot_args, BOOT_ARGS_SIZE, &add).is_err() {
            return Err(ConfigError::new(format!(
                "boot-args-add does not fit beside the guest boot args ({} bytes at most)",
                BOOT_ARGS_SIZE - 1
            )));
        }
        if self.haptics_native_impacts == Some(true)
            && native_impacts_boot_arg(&mut self.boot_args, BOOT_ARGS_SIZE).is_err()
        {
            return Err(ConfigError::new(
                "haptics-native-impacts=1: the boot arguments say oneios_native_impacts= otherwise, or are too long",
            ));
        }
        if self.disks.is_empty() {
            self.disks = image.disks.clone();
        }
        if self.kernel.is_empty() {
            match exists(&self.image, "kernel.kc").or_else(|| exists(&self.image, "kernel.macho")) {
                Some(path) => self.kernel = path,
                None => return Err(ConfigError::new(format!("no kernel.kc in {} (and no --kernel)", self.image))),
            }
        }
        if self.identity.is_empty() {
            self.identity = exists(&self.image, "identity.adt").unwrap_or_default();
        }
        if self.disks.is_empty() {
            self.image_disks()?;
        }
        self.finish_ram(&image, can_lock)
    }

    /// The image set's volumes (scripts/image/build.sh): read-only APFS containers, in this order; the Data store; the
    /// Data seed.
    fn image_disks(&mut self) -> Result<(), ConfigError> {
        for name in ["os.img", "systemos.img", "app.img"] {
            if let Some(path) = exists(&self.image, name) {
                self.disks.push(DiskConfig { path, ..DiskConfig::default() });
            }
        }
        let seed = exists(&self.image, "data.seed");
        if !self.data.is_empty() {
            let path = format!("{}/data.img", self.data);
            if path.len() >= 512 {
                return Err(ConfigError::silent());
            }
            self.disks.push(DiskConfig {
                path,
                writable: true,
                data_store: true,
                seed: seed.clone().unwrap_or_default(),
            });
        }
        // The Data seed is a tar (docs/image-format.md): Linux cannot write APFS, so the guest makes the Data volume
        // itself on the first start and unpacks the seed, which it reads as the last read-only disk. (An APFS seed
        // image is copied into the store instead: blk.rs.)
        if let Some(path) = seed {
            self.disks.push(DiskConfig { path, ..DiskConfig::default() });
        }
        Ok(())
    }

    /// The guest's RAM: the command line's ram=, else the image set's, else auto; the lock, the valve, what fits.
    fn finish_ram(&mut self, image: &Config, can_lock: &dyn Fn() -> bool) -> Result<(), ConfigError> {
        take!(self, image, "ram", ram_bytes);
        take!(self, image, "ram-view", ram_view); // docs/booter.md 2.2 "The memory view"
        take!(self, image, "ram-percent", ram_percent);
        take!(self, image, "ram-host-floor", ram_host_floor);
        take!(self, image, "host-mem-log", host_mem_log);
        take!(self, image, "usb-log", usb_log);
        // Default 0 (today's immediate kill): the graceful path's guest half (iOS actually powering off) is not yet
        // built or tested end to end, so it is off until the Mac/phone proves it. return-graceful-ms=N (or the image's
        // xnuhost.conf) turns it on. The host state machine itself is tested (docs/booter.md "The return").
        take!(self, image, "return-graceful-ms", return_graceful_ms);
        // session/ready at the first lit frame of the guest's own, or this many seconds after its first (black) one.
        take!(self, image, "ready-content-wait", ready_content_wait);
        if self.ram_meminfo.is_empty() {
            self.ram_meminfo =
                if image.ram_meminfo.is_empty() { "/proc/meminfo".to_owned() } else { image.ram_meminfo.clone() };
        }
        // The RAM held fixed and locked for the session (ramfix.rs): the default since fix-ram-locked (2026-11-15; the
        // owner's rule: iOS's RAM is never swapped). The phone's session of main 3233fed showed why: with ram-giveback=1
        // at a fitted 6656 MiB Linux still swapped iOS's RAM (shmem) with 1.9 GB of other page cache there, 138 -> 604
        // MiB at up to 37.9 MB/s of swap-outs, and only a lock stops that. ram-giveback=1 is 0058's way again.
        take!(self, image, "ram-giveback", ram_giveback);
        // Option C (2026-11-23, off unless asked): faulted in as used, each page locked as it comes in, free pages given
        // back: ram-giveback=1's way with a lock on fault (session/memory.rs ram_fix_start). Never prefaulted.
        take!(self, image, "ram-ondemand", ram_ondemand);
        if self.ram_ondemand {
            self.ram_giveback = true;
        }
        if !self.given("ram-prefault") {
            self.ram_prefault = if image.given("ram-prefault") { image.ram_prefault } else { !self.ram_giveback };
        }
        if !self.given("ram-lock") {
            self.ram_lock = if image.given("ram-lock") { image.ram_lock } else { !self.ram_giveback };
        }
        if self.ram_giveback {
            // a RAM that gives pages back is neither prefaulted nor locked
            self.ram_prefault = false;
            self.ram_lock = false;
        }
        if !self.ram_prefault {
            self.ram_lock = false;
        }
        take!(self, image, "ram-unlock-below", ram_unlock_below);
        // A locked RAM always has its valve: under it the host may swap the guest again rather than reclaim every other
        // process's pages (the fork's watcher's, surfaceflinger's) on the way to its OOM killer. The way back must fit.
        if (self.ram_lock || self.ram_ondemand) && self.ram_unlock_below < RAM_UNLOCK_MIN {
            eprintln!(
                "xnuhost: ram-unlock-below={} MiB is under {} MiB with the RAM locked: {} MiB is used (the valve stays on)",
                self.ram_unlock_below >> 20,
                RAM_UNLOCK_MIN >> 20,
                RAM_UNLOCK_MIN >> 20
            );
            self.ram_unlock_below = RAM_UNLOCK_MIN;
        }
        // The headroom: smaller with a lock that can work (no zram pool or working set for the guest's pages), which is
        // asked here, before the size is known (ramfix::can_lock: CAP_IPC_LOCK, or no RLIMIT_MEMLOCK).
        take!(self, image, "ram-fit-terms", ram_fit_terms_old);
        // ram-ondemand locks each page as it comes in: the same headroom as a whole lock.
        self.ram_lock_expected = (self.ram_lock || self.ram_ondemand) && can_lock();
        if !self.given("ram-session-reserve") {
            self.ram_session_reserve = if image.given("ram-session-reserve") {
                image.ram_session_reserve
            } else {
                match (self.ram_lock_expected, self.ram_fit_terms_old) {
                    (true, false) => RAM_SESSION_RESERVE_LOCKED,
                    (false, false) => RAM_SESSION_RESERVE,
                    (true, true) => RAM_SESSION_RESERVE_LOCKED_OLD,
                    (false, true) => RAM_SESSION_RESERVE_OLD,
                }
            };
        }
        // With the lock, what fits leaves at least the valve's level and a margin: else the valve would open as soon
        // as the session is up.
        if (self.ram_lock || self.ram_ondemand) && self.ram_session_reserve < self.ram_unlock_below + RAM_VALVE_MARGIN {
            self.ram_session_reserve = self.ram_unlock_below + RAM_VALVE_MARGIN;
        }
        take!(self, image, "ram-fit", ram_fit_on);
        take!(self, image, "ram-huge", ram_huge);
        take!(self, image, "ram-gpu-reserve", ram_gpu_reserve);
        take!(self, image, "ram-pagetable-percent", ram_pagetable_percent);
        take!(self, image, "ram-hold-percent", ram_hold_percent);
        take!(self, image, "ram-tasks", ram_tasks);
        take!(self, image, "ram-task-cost", ram_task_cost);
        take!(self, image, "ram-tasks-awake", ram_tasks_awake);
        take!(self, image, "ram-task-idle-cost", ram_task_idle_cost);
        // ram-fit-terms=old: the terms before 2026-11-23 for each one neither the command line nor the image gives.
        if self.ram_fit_terms_old {
            let unset: Vec<bool> = ["ram-pagetable-percent", "ram-hold-percent", "ram-tasks"]
                .iter()
                .map(|k| !self.given(k) && !image.given(k))
                .collect();
            if unset[0] {
                self.ram_pagetable_percent = RAM_PAGETABLE_PERCENT_OLD;
            }
            if unset[1] {
                self.ram_hold_percent = 0;
            }
            if unset[2] {
                self.ram_tasks = 0;
            }
        }
        // What the host can hold for the guest without swapping it (2026-11-14, fix-ram-fit): MemAvailable now, after
        // the fork stopped One UI's zygote and before the guest takes anything (oneos/interface.md 4), less what the
        // session needs outside the guest at its peak (2026-11-23): what holding the RAM costs (a share of it), the iOS
        // tasks' page tables and kernel memory (per task), KGSL's own memory, and the headroom the host keeps. With the RAM locked the host can neither swap nor reclaim it, so this is the bound; the page
        // cache MemAvailable counts is taken (the host reclaims cache, never the locked RAM).
        let known = meminfo(&self.ram_meminfo);
        let (total, available) = known.unwrap_or((0, 0));
        self.ram_fit = if known.is_some() { ram_fit(available, &self.fit_terms()) } else { 0 };
        self.ram_fit_available = available;
        self.ram_lock_fit = if self.ram_lock_expected { self.ram_fit & !(STEP_256M - 1) } else { 0 };
        if self.ram_bytes == 0 {
            if known.is_none() {
                // Not Linux, or no /proc: the size of the time before ram=auto.
                eprintln!("xnuhost: ram=auto: no MemTotal in {}: the guest gets 4 GiB", self.ram_meminfo);
                self.ram_bytes = 4 << 30;
            } else {
                let fit = if self.ram_fit_on { Some(self.ram_fit) } else { None };
                let (ram, bound) = ram_fit_auto(total, self.ram_percent, self.ram_host_floor, fit);
                if ram == 0 {
                    return Err(ConfigError::new(format!(
                        "ram=auto: this host has {} MiB, {} MiB of it available; {}% of it, all but {} MiB, or what fits \
                         beside the session's needs ({} MiB), is less than the guest's 1 GiB: give --ram SIZE",
                        total >> 20,
                        available >> 20,
                        self.ram_percent,
                        self.ram_host_floor >> 20,
                        self.ram_fit >> 20
                    )));
                }
                self.ram_bytes = ram;
                self.ram_auto_bound = bound;
                self.ram_auto_total = total;
                self.ram_auto_available = available;
            }
        }
        if self.ram_bytes & 0x1fffff != 0 || self.ram_bytes < STEP_256M || self.ram_bytes > RAM_MAX {
            return Err(ConfigError::new(format!(
                "--ram must be a multiple of 2 MiB, at least 256 MiB, at most {} GiB",
                RAM_MAX >> 30
            )));
        }
        Ok(())
    }

    /// The host phone's layer (onebridges::hw; docs/hardware.md, "The host device"): the returned config holds the table
    /// entry's xnuhost settings over the defaults, and self.host and self.host_line say what was found and what of it
    /// is taken. Only the keys of [`device_key`] are merged as device defaults; a table entry with another key is
    /// refused here, so a new default cannot be added to the table without its merge (the tests check every entry).
    fn host_device_layer(&mut self, image: &Config) -> Result<Config, ConfigError> {
        let mut device = Config::new();
        if self.host_device.is_empty() {
            self.host_device = if image.host_device.is_empty() { "auto".to_owned() } else { image.host_device.clone() };
        }
        if self.host_props.is_empty() {
            self.host_props = image.host_props.clone();
        }
        if self.host_root.is_empty() {
            self.host_root = image.host_root.clone();
        }
        self.host = None;
        let (entry, facts, why) =
            bhw::host_choose(&self.host_device, &self.host_props, &self.host_root).map_err(|why| {
                ConfigError::new(format!("{why} (host-device: auto, none or one of {})", bhw::host_names()))
            })?;
        let Some(entry) = entry else {
            self.host_line = if self.host_device.eq_ignore_ascii_case("none") {
                "host: detection off (host-device=none): today's defaults".to_owned()
            } else {
                format!(
                    "host: unknown phone (read: {}): today's defaults (host-device=NAME forces one of {})",
                    bhw::host_describe(&facts),
                    bhw::host_names()
                )
            };
            return Ok(device);
        };
        self.host = Some(Host(entry));
        let mut over = self.hw;
        over.merge(&image.hw);
        let mut taken = Vec::new();
        for pair in entry.config.iter() {
            let bad = |what: &str| ConfigError::new(format!("host table: {}: {pair} is {what}", entry.name));
            let Some((key, value)) = pair.split_once('=').filter(|(k, _)| k.len() < 64) else {
                return Err(bad("not KEY=VALUE"));
            };
            if !device_key(key) || device.set(key, value).is_err() {
                return Err(bad("not a device default xnuhost takes"));
            }
            if self.given_above(image, &over, key, value) {
                taken.push(format!("{pair} not taken (the command line or the image set gives it)"));
            } else {
                taken.push((*pair).to_owned());
            }
        }
        let taken = taken.join(", ");
        let env = bhw::host_env_describe(entry);
        let defaults = match (taken.is_empty(), env.is_empty()) {
            (true, true) => "none (today's)".to_owned(),
            (false, false) => format!("{taken}, {env}"),
            _ => format!("{taken}{env}"),
        };
        self.host_line = format!(
            "host: {} ({}), {why}; read: {}; defaults: {defaults}",
            entry.name,
            entry.label,
            bhw::host_describe(&facts)
        );
        Ok(device)
    }

    /// --host-info (session::main): the host table's row for this phone, the one line (without its line break) for the
    /// OneOS switch's preflight (oneos/interface.md 4a): "host-info: device=galaxy-s24-ultra takeover=yes (matched by
    /// ro.product.model=SM-S928B)", or "host-info: device=none takeover=no (unknown phone; read: ...)". It reads what
    /// the finish's host layer reads (the properties, three sysfs and proc files; host-device=, host-props=, host-root=
    /// from the command line or a --config file) and needs no image. Err: a property file that cannot be read (a
    /// host-device= that names no row is refused when it is set).
    pub fn host_info_line(&self) -> Result<String, ConfigError> {
        let (entry, facts, why) =
            bhw::host_choose(&self.host_device, &self.host_props, &self.host_root).map_err(ConfigError::new)?;
        Ok(match entry {
            Some(e) => {
                format!("host-info: device={} takeover={} ({why})", e.name, if e.takeover { "yes" } else { "no" })
            }
            None => format!("host-info: device=none takeover=no ({why}; read: {})", bhw::host_describe(&facts)),
        })
    }

    /// Whether the session reads the side keys: every session does; only the bench preview's grab (touch-exclusive=1
    /// with mode=bench) leaves them to the Android that runs under it (input::start's `read_keys`).
    pub fn reads_side_keys(&self) -> bool {
        !(self.touch_exclusive && self.mode == Mode::Bench)
    }

    /// What the "hw: touch: ..." line says of the grab ("off", the bench preview's, the takeover's).
    pub fn exclusive_touch_text(&self) -> &'static str {
        if !self.touch_exclusive {
            "off"
        } else if self.mode == Mode::Bench {
            "requested (bench+SHM sec_touchscreen grab)"
        } else {
            "requested (session-mode=takeover: sec_touchscreen grab, the side keys read and never grabbed)"
        }
    }

    /// The session's line in the opt-in mode only (None in session-mode=hosted: its log has no such line;
    /// docs/booter.md 2.9d).
    pub fn takeover_line(&self) -> Option<String> {
        (self.session_mode == SessionMode::Takeover).then(|| {
            // The switch's memory steps, named only when they are not the default (takeover-reclaim=1, keep).
            let mut memory = String::new();
            if !self.takeover_reclaim {
                memory.push_str("; takeover-reclaim=0 (the switch drops no cache)");
            }
            if let Some(n) = self.takeover_swappiness {
                memory.push_str(&format!("; takeover-swappiness={n} (vm.swappiness for the session, put back by the return)"));
            }
            format!(
                "session: Android takeover (session-mode=takeover; {} allows it): the native touchscreen is grabbed for iOS{}; the side keys are read, never grabbed; Android's services stopped by the switch: takeover-stops={}{}; the switch's CPU cap, its stops and the way back are the OneOS fork's",
                self.host.map_or("?", |h| h.0.name),
                if self.touch_exclusive { "" } else { " (not: touch-exclusive=0 was given)" },
                if self.takeover_stops.is_empty() { "default" } else { &self.takeover_stops },
                memory
            )
        })
    }

    /// Whether the command line or the image set (`over`, the two already merged for hw) gives the key of `key=value`.
    fn given_above(&self, image: &Config, over: &HwSettings, key: &str, value: &str) -> bool {
        match key {
            "gpu" => self.given("gpu") || image.given("gpu"),
            "panel-fallback" => self.panel_fallback.is_some() || image.panel_fallback.is_some(),
            "hz" => self.hz != 0 || image.hz != 0,
            // the frame is taken as one (width, height, scale): not where a width or a height is given above
            "width" | "height" | "scale" => {
                self.width != 0
                    || self.height != 0
                    || image.width != 0
                    || image.height != 0
                    || (key == "scale" && self.given("scale"))
            }
            "return-graceful-ms" | "astc" | "corner-radius" | "artwork-subtype" | "thermal-cap" | "pen" | "encoder" => {
                self.given(key) || image.given(key)
            }
            "boot-args-add" => false,
            _ => {
                let mut one = HwSettings::default();
                if one.setting(key, value).is_err() {
                    return false;
                }
                (one.all != Backend::Unset && over.all != Backend::Unset)
                    || (one.fallback.is_some() && over.fallback.is_some())
                    || one.one.iter().zip(over.one.iter()).any(|(a, b)| *a != Backend::Unset && *b != Backend::Unset)
            }
        }
    }

    // ---- the display's mode (docs/booter.md 2.8) --------------------------------------------------------------------

    /// xh_panel_fallback: the mode assumed where the panel cannot be read: panel-fallback= where set, else 1440x3120 at
    /// 120 Hz.
    pub fn panel_fallback(&self) -> PanelMode {
        match self.panel_fallback {
            Some(m) if m.width != 0 && m.height != 0 => PanelMode { hz: if m.hz != 0 { m.hz } else { PANEL_HZ }, ..m },
            _ => PanelMode { width: PANEL_WIDTH, height: PANEL_HEIGHT, hz: PANEL_HZ },
        }
    }

    /// xh_display_mode: the guest's mode: width, height and hz where configured (command line, then the image set's
    /// xnuhost.conf), else the mode of the panel (None: not known), else the fallback.
    pub fn display_mode(&self, panel: Option<&PanelMode>) -> PanelMode {
        let mut mode = self.panel_fallback();
        if let Some(p) = panel.filter(|p| p.width != 0 && p.height != 0) {
            mode = *p;
            if !(24..=240).contains(&mode.hz) {
                mode.hz = self.panel_fallback().hz;
            }
        }
        if self.width != 0 {
            mode.width = self.width;
        }
        if self.height != 0 {
            mode.height = self.height;
        }
        if self.hz != 0 {
            mode.hz = self.hz;
        }
        mode
    }
}

/// The boot arguments when neither the command line nor the image set gives any.
pub const DEFAULT_BOOT_ARGS: &str =
    "debug=0x14e serial=3 -v kt_fixed_zones=120 -no_slto_panic ml-timeout-lock-panic-scale=40 tlto_us=0 \
sheng-smp=1 armcpu-self-ipi=1 armcpu-spi-ipi=1 use_contiguous_hint=0 interrupt-masked-debug-mode=0 \
sched_preemption_disable_debug_mode=0 wdt=-1 native-source-storage=1 vgpu-3d=1 droidvm-gpu-metal=1";

/// The keys a host table entry may give as device defaults.
fn device_key(key: &str) -> bool {
    matches!(
        key,
        "gpu"
            | "panel-fallback"
            | "corner-radius"
            | "artwork-subtype"
            | "boot-args-add"
            | "thermal-cap"
            | "hz"
            | "width"
            | "height"
            | "scale"
            | "return-graceful-ms"
            | "astc"
            | "pen"
            | "encoder"
    ) || key.starts_with("hw-backend")
}

/// `<dir>/<name>` when it is a regular file.
fn exists(dir: &str, name: &str) -> Option<String> {
    let path = format!("{dir}/{name}");
    if path.len() >= 512 {
        return None;
    }
    fs::metadata(&path).ok().filter(|m| m.is_file()).map(|_| path)
}

/// The space-separated words of boot arguments.
fn words(args: &str) -> impl Iterator<Item = &str> {
    args.split(' ').filter(|w| !w.is_empty())
}

/// Appends ` word` (no space when `args` is empty) when it fits a C buffer of `size` bytes.
fn append_arg(args: &mut String, size: usize, word: &str) -> Result<(), NoRoom> {
    let gap = usize::from(!args.is_empty());
    if args.len() + gap + word.len() >= size {
        return Err(NoRoom);
    }
    if gap == 1 {
        args.push(' ');
    }
    args.push_str(word);
    Ok(())
}

/// haptics-native-impacts=1 (docs/hardware.md 5, "Native impacts"): the guest's half of the opt-in,
/// oneios_native_impacts=1, added to the boot arguments as they are (never in place of them). Err when they already
/// say oneios_native_impacts= with another value or twice, or there is no room.
fn native_impacts_boot_arg(args: &mut String, size: usize) -> Result<(), NoRoom> {
    const KEY: &str = "oneios_native_impacts=";
    let mut seen = false;
    for w in words(args).filter(|w| w.starts_with(KEY)) {
        if seen || w != "oneios_native_impacts=1" {
            return Err(NoRoom);
        }
        seen = true;
    }
    if seen {
        return Ok(());
    }
    append_arg(args, size, "oneios_native_impacts=1")
}

/// The boot arguments an iOS 16.5 (20F66) userland needs on the 24A446 kernel collection beyond the image set's own
/// (roottweak's 20F66 xnuhost.conf), from the Galaxy A56 fork's switch (Musicalista/OneiOS-A56 e8747cc, c83b9ca;
/// docs/a56.md). xnuhost adds them for an image set of build 20F66 only, so the user updates the runtime and the
/// kernel and keeps the image: oneios-hid16=1 (kernel 0103: touch events in 16.5's layout), oneios-grant=configd's
/// IONetworkStack entitlement (0104: en0 is named, Wi-Fi comes up), oneios-ls-keep=3 (0105: LaunchServices' database
/// kept), oneios-disable-jobs (0053: 0053's three defaults and the fork's eight idle jobs; 250 bytes, the kernel's list
/// holds 255).
pub const IOS16_BOOT_ARGS: &str = "oneios-hid16=1 \
oneios-grant=configd:com.apple.networking.ionetworkstack.user-client oneios-ls-keep=3 \
oneios-disable-jobs=com.apple.accessibility.axassetsd,com.apple.aonsensed,com.apple.prngseedd,com.apple.suggestd,\
com.apple.spotlightknowledged,com.apple.corespotlightservice,com.apple.siriknowledged,com.apple.tipsd,com.apple.gamed,\
com.apple.followupd,com.apple.tailspind";

/// The words of [`IOS16_BOOT_ARGS`] that `args` does not name already, added (the command line's and the image set's
/// win); the names of those added, space-separated (empty: all were named). Err when they do not fit `size` bytes.
pub fn ios16_boot_args(args: &mut String, size: usize) -> Result<String, NoRoom> {
    let mut next = args.clone();
    let mut added = Vec::new();
    for w in words(IOS16_BOOT_ARGS) {
        let name = w.split('=').next().unwrap_or(w);
        if !boot_arg_named(&next, name) {
            append_arg(&mut next, size, w)?;
            added.push(name);
        }
    }
    *args = next;
    Ok(added.join(" "))
}

/// Whether `args` has a boot argument named `name`: a word equal to it, or one that starts with it and then '='.
fn boot_arg_named(args: &str, name: &str) -> bool {
    words(args).any(|w| w.strip_prefix(name).is_some_and(|rest| rest.is_empty() || rest.starts_with('=')))
}

/// boot-args-add (ported from YumikoHayamato/OneiOS, s25-ultra-sm8750 440abb2): each of its words is added to `args`
/// unless `args` names that argument already, so what the command line or the image set wrote in boot-args wins, and a
/// profile adds one argument (oneios-disable-jobs=..., oneios-ubt=...) without repeating the defaults: a boot-args= line
/// that replaced them was the cause of logo-only stalls on the S25 Ultra (docs/s25.md). Err when the result does not fit
/// a C buffer of `size` bytes.
pub fn add_boot_args(args: &mut String, size: usize, add: &str) -> Result<(), NoRoom> {
    for w in words(add) {
        let name = w.split('=').next().unwrap_or(w);
        if !boot_arg_named(args, name) {
            append_arg(args, size, w)?;
        }
    }
    Ok(())
}

/// Whether `args` has `name=` as a word's start (at the start of the text or after a space).
fn has_switch(args: &str, name: &str) -> bool {
    args.match_indices(name).any(|(at, _)| {
        (at == 0 || args.as_bytes()[at - 1] == b' ') && args.as_bytes().get(at + name.len()) == Some(&b'=')
    })
}

/// xh_boot_args_hides_vmm_from: whether the boot arguments' owner-hide-vmm=NAME[,NAME...] (the kernel's per-process
/// vmm-present switch) names `name`. xnuhost warns for bluetoothd: 24A446's bluetoothd crashes in a loop with the
/// virtual machine hidden.
pub fn boot_args_hides_vmm_from(args: &str, name: &str) -> bool {
    const KEY: &str = "owner-hide-vmm=";
    args.match_indices(KEY).any(|(at, _)| {
        if at != 0 && args.as_bytes()[at - 1] != b' ' {
            return false;
        }
        let list = &args[at + KEY.len()..];
        let list = list.split(' ').next().unwrap_or("");
        list.split(',').any(|entry| entry == name)
    })
}

/// xh_boot_args_hide_vmm_add: adds `name` to the boot arguments' owner-hide-vmm= list, the first such word (the one
/// the kernel's PE_parse_boot_arg_str reads), or a new word when there is none. `Ok(true)`: added, `Ok(false)`: the
/// list names it already; `NoRoom`: no room in the arguments (`size` with C's NUL) or in the kernel's 127 characters
/// for the list (bsd/kern/hvg_sysctl.c).
pub fn boot_args_hide_vmm_add(args: &mut String, size: usize, name: &str) -> Result<bool, NoRoom> {
    const KEY: &str = "owner-hide-vmm=";
    const LIST_MAX: usize = 127;
    let (used, n) = (args.len(), name.len());
    let word = args.match_indices(KEY).map(|(at, _)| at).find(|&at| at == 0 || args.as_bytes()[at - 1] == b' ');
    let Some(at) = word else {
        if used + usize::from(used != 0) + KEY.len() + n + 1 > size || n > LIST_MAX {
            return Err(NoRoom);
        }
        if used != 0 {
            args.push(' ');
        }
        args.push_str(KEY);
        args.push_str(name);
        return Ok(true);
    };
    let first = args[at..].split(' ').next().unwrap_or("");
    if first.len() >= 256 {
        return Err(NoRoom); // C's char first[256]
    }
    if boot_args_hides_vmm_from(first, name) {
        return Ok(false);
    }
    let start = at + KEY.len();
    let list = first.len() - KEY.len();
    if used + 1 + n + 1 > size || list + 1 + n > LIST_MAX {
        return Err(NoRoom);
    }
    // "owner-hide-vmm=" with an empty list: no leading comma
    let add = if list == 0 { name.to_owned() } else { format!(",{name}") };
    args.insert_str(start + list, &add);
    Ok(true)
}

/// xh_boot_args_bt_transport: the transport the boot arguments' bt-transport-override gives 24A446's bluetoothd, read
/// the way bluetoothd reads it (/usr/sbin/bluetoothd at 0x100309098): every space-separated word that contains
/// "bt-transport-override" sets the transport to strtol of what follows its '=', when under 13; the last such word
/// wins. None when they give none. xnuhost warns for any value but 10: in a VM bluetoothd powers on only with 10 (the
/// guest's driver gives it 10 and the kernel BT_HCI_TRANSPORT=H4:POSIX, docs/hardware.md 4).
pub fn boot_args_bt_transport(args: &str) -> Option<u32> {
    let mut found = None;
    for w in args.split(' ') {
        if !w.contains("bt-transport-override") {
            continue;
        }
        if let Some((_, value)) = w.split_once('=') {
            let v = parse::strtol(value, 10).map_or(0, |(v, _)| v);
            if (0..13).contains(&v) {
                found = Some(v as u32);
            }
        }
    }
    found
}

/// xh_display_boot_args: the display's switches the guest needs, unless the command line has them: its UI scale
/// (offline-legacy-unit-scale, /chosen display-scale), the panel's rate (sheng-vsync: the vsync tick and, above 60 Hz,
/// the refresh properties), one fixed rate above 60 Hz (sheng-vsync-fixed), and with a half-float frame
/// (display-format=rgba16f) oneios-display-wide=1, which tells the guest's display to hand half-float surfaces to the
/// scanout as they are instead of narrowing them to 8 bits. Err with no room in a C buffer of `size` bytes (what was
/// added before stays).
pub fn display_boot_args(
    args: &mut String,
    size: usize,
    scale: u32,
    hz: u32,
    format: DisplayFormat,
) -> Result<(), NoRoom> {
    if format == DisplayFormat::Rgba16f && !has_switch(args, "oneios-display-wide") {
        append_arg(args, size, "oneios-display-wide=1")?;
    }
    if !has_switch(args, "offline-legacy-unit-scale") {
        append_arg(args, size, &format!("offline-legacy-unit-scale={scale}"))?;
    }
    if !has_switch(args, "sheng-vsync") {
        append_arg(args, size, &format!("sheng-vsync={hz}"))?;
    }
    // The panel refreshes at one rate; its driver's original maximum refresh interval (1/24 s) is ProMotion's range,
    // not this panel's. The render server reads the range and lets CoreAnimation pick 30 Hz for an animation inside
    // it; a fixed rate makes the animation ask for the panel's (docs/gpu.md, "The refresh range"). A given
    // sheng-vsync-fixed stays (0: the variable range set by sheng-vsync-min, 60..120 by default).
    if hz > 60 && !has_switch(args, "sheng-vsync-fixed") {
        append_arg(args, size, "sheng-vsync-fixed=1")?;
    }
    Ok(())
}

/// xh_view_boot_args, ram-view= (docs/booter.md 2.2, "The memory view"): the compressor's room for a view of `view`
/// bytes over `window` bytes of RAM, unless the command line sets it. XNU caps what the compressor holds at its pool's
/// pages times kern.compressor_pool_multiplier, and at least max_mem's pages (vm_compressor_set_size; 1 on an iOS of
/// more than 2 GiB): so at most the RAM's own size of original pages. The multiplier is the view over the RAM, rounded
/// up (2 for 12 GiB over 7 GiB). Ok(multiplier) (0: no view, nothing added), Err with no room.
pub fn view_boot_args(args: &mut String, size: usize, view: u64, window: u64) -> Result<u32, NoRoom> {
    if view == 0 || window == 0 || view <= window {
        return Ok(0);
    }
    let multiplier = view.div_ceil(window) as u32;
    if !has_switch(args, "kern.compressor_pool_multiplier") {
        append_arg(args, size, &format!("kern.compressor_pool_multiplier={multiplier}"))?;
    }
    Ok(multiplier)
}

/// The @3x fixups of -[CADisplayMode preferredScale] per iOS build (scale_fixups): the build, and its entries
/// (subcache, file offset, original bytes, new bytes). One function, the same 32 instructions in each build, only its
/// place in dyld_shared_cache_arm64e.01 differs (docs/userland.md, "The UI scale", has how each was found).
/// - 24A446: the function at 0x40b1758 (0x1844b1758), entry 11 of QuartzCore's -[CADisplayMode] method list.
/// - 24B156 (27.2.1): at 0x3f9d6f4 (0x18439d6f4), entry 11 of the same list (its 31 selectors are 24A446's), found by
///   the function's own bytes (one match in .01) and checked by its selector name; the original bytes are the same.
pub const SCALE_FIXUPS_3X: &[(&str, &[&str])] = &[
    (
        "24A446",
        &[
            "dyld_shared_cache_arm64e.01,40b17a8,49008052,69008052", // mov w9, #2 -> mov w9, #3
            "dyld_shared_cache_arm64e.01,40b17bc,280080520895889a,680080521f2003d5", // mov w8, #1; cinc x8, x8, hi -> mov w8, #3; nop
        ],
    ),
    (
        "24B156",
        &[
            "dyld_shared_cache_arm64e.01,3f9d744,49008052,69008052", // mov w9, #2 -> mov w9, #3
            "dyld_shared_cache_arm64e.01,3f9d758,280080520895889a,680080521f2003d5", // mov w8, #1; cinc x8, x8, hi -> mov w8, #3; nop
        ],
    ),
];

/// xh_scale_fixups: -[CADisplayMode preferredScale] of QuartzCore, read in 24A446's cache and 24B156's
/// (docs/userland.md, "The UI scale"): a mode with no preferred scale of its own is @2x in a virtual device (or what
/// the mode's own field says, when more), else @2x when taller than 1080 pixels and @1x otherwise. Nothing there
/// answers 3, so for a scale of 3 both answers are changed: the virtual device's floor, and the height test's result.
/// A scale of 2 is what the code gives unchanged. The entries are the image set's build's (SCALE_FIXUPS_3X; none for
/// a build not there). The kernel applies a fixup only where the original bytes match. Appends to `list` what fits.
pub fn scale_fixups(build: Option<&str>, scale: u32, list: &mut TextFixups) {
    if scale != 3 {
        return;
    }
    let Some((_, fixups)) = SCALE_FIXUPS_3X.iter().find(|(b, _)| Some(*b) == build) else {
        return;
    };
    for f in fixups.iter() {
        if !list.push_within(f, TEXT_FIXUPS_SIZE) {
            break;
        }
    }
}

/// Whether the guest's boot arguments give the Galaxy A56's processes OpenGL ES: oneios-gpu-amd-gl=1, the first
/// oneios-gpu-amd-gl= word, as the kernel's PE_parse_boot_argn reads it (docs/a56-gpu.md 4a: the accelerator then names
/// OneiOSRadeonGL as IOGLESBundleName for every process).
pub fn amd_gl_on(boot_args: &str) -> bool {
    boot_args.split(' ').find(|w| w.starts_with("oneios-gpu-amd-gl=")) == Some("oneios-gpu-amd-gl=1")
}

/// Whether this boot has the A56's OpenGL ES driver, so that CoreImage's GL fallback needs no fixup (ios16_fixups):
/// xnuhost found OneiOSRadeonGL.bundle in the gpuaddon of the amdgpu renderer it picked (`bundle`) and the guest's
/// boot arguments turn it on. A hand-written oneios-gpu-amd-gl=1 without the bundle keeps the fixup: the kernel would
/// name a bundle that is not there, CoreImage would get no EAGLContext and die as before.
pub fn ios16_gl_driver(bundle: bool, boot_args: &str) -> bool {
    bundle && amd_gl_on(boot_args)
}

/// The text fixups an iOS 16.5 (20F66) userland needs (docs/gpu.md, "iOS 16.5 without a Metal device"). Read in
/// 20F66's dyld_shared_cache_arm64 (out/ios16-cache, `oneios-tool sym find` / `sym callers`): CoreImage's
/// -[CIContext initWithOptions:] makes a Metal context when MTLCreateSystemDefaultDevice has a device, and else falls
/// back to +[CIContext internalGLContextWithOptions:] (0x1949bd62c unslid, reached from nowhere else), an OpenGL ES
/// context through internalContextWithEAGLContext:nil. On iOS 16 there is no CPU renderer (kCIContextUseSoftwareRenderer
/// still takes GL) and CIGLIsUsable answers 1 always. With no GL driver (the Galaxy A56: no Apple GPU, no GL of ours)
/// the context has no EAGL macro context, GetMacroContext leaves its dispatch table zero, and CI::GLContext::init calls
/// glDisable(GL_DEPTH_TEST) through it: pc 0, lr CI::GLContext::init + 0xcc. SpringBoard died so 9 times in the A56's
/// boot of 2026-11-24 21:47, at each start after Bold Text was turned on (UIKit's styled images: +[CUIShapeEffectStack
/// sharedCIContext], iOSPlus10 docs/boldtext-crash.md, the same fault on 20H392); any app reaching that context can.
/// The fixup makes the GL fallback answer no context (mov x0, #0; ret): initWithOptions: then returns nil, as it does
/// when GL is not usable, and CoreUI renders into a blank bitmap through a nil context (-[CUIShapeEffectStack
/// _newFlattenedImageFromShapeCGImage:...] sends render:toBitmap: to it) instead of crashing. A Metal device (the
/// Metal plug-in, docs/a56-gpu.md) is taken before the fallback, so it keeps working. Appends to `list` what fits and
/// is not there yet; the kernel applies an entry only where its original bytes match. The number of entries added.
pub fn ios16_fixups(build: Option<&str>, list: &mut TextFixups) -> usize {
    const FIXUPS_20F66: &[&str] = &[
        // +[CIContext internalGLContextWithOptions:]: mov x3, x2; adrp x8, ... -> mov x0, #0; ret
        "dyld_shared_cache_arm64.07,a162c,e30302aa88512490,000080d2c0035fd6",
    ];
    if build != Some("20F66") {
        return 0;
    }
    let mut added = 0;
    for f in FIXUPS_20F66 {
        if list.entries().iter().any(|e| e == f) {
            continue;
        }
        if !list.push_within(f, TEXT_FIXUPS_SIZE) {
            break;
        }
        added += 1;
    }
    added
}

/// iOS 16.5's (20F66) product: the iPhone X's /product product-id (DeviceTree.d22ap of the restore file the image is
/// made from, iPhone10,3/iPhone10,6 16.5, read with `oneios-tool remotezip get`, `image im4p` and `xnuloader-inspect
/// adt`), entry 36 of 20F66 libMobileGestalt's table of 363 products (product type 0x669eebc5). MobileGestalt finds
/// the product by it; without it MGGetProductType is -1, Settings has no model name, and bluetoothd's chip factory
/// (MGIsDeviceOneOfType against lists of these ids) leaves its chip null and dies on it (status 11 every 10 s:
/// [chip + 0x260] at 0x100274cf0 of 20F66's /usr/sbin/bluetoothd). With it the factory makes the iPhone X's chip (15).
/// The root's model and target-type stay the generic machine's (docs/userland.md, "The iOS 16.5 identity").
pub const IOS16_PRODUCT_ID: [u8; 20] = [
    0xfd, 0x07, 0x9b, 0x29, 0x81, 0x44, 0xa7, 0xde, 0xb4, 0xd7, 0xc8, 0x7f, 0xef, 0x3e, 0xb4, 0x74, 0xd8, 0xf0, 0xe7,
    0xeb,
];

/// The serial number an iOS 16.5 guest shows (Settings > General > About): OneiOS's own, made up, the same on every
/// phone; never an Apple serial. 11 characters, the most XNU's IODTPlatformExpert gives from serial-number.
pub const IOS16_SERIAL: &str = "ONEIOS16A56";

/// xh_panel_native: the panel's native mode among the modes of its connector: the largest, at its highest refresh
/// rate. Not the current one: One UI's resolution setting may run the panel at 1080x2340.
pub fn panel_native(modes: &[PanelMode]) -> Option<PanelMode> {
    let mut native: Option<PanelMode> = None;
    for m in modes.iter().filter(|m| m.width != 0 && m.height != 0 && m.width <= 16384 && m.height <= 16384) {
        let area = m.width as u64 * m.height as u64;
        let better = match native {
            None => true,
            Some(n) => {
                let best = n.width as u64 * n.height as u64;
                area > best || (area == best && m.width == n.width && m.hz > n.hz)
            }
        };
        if better {
            native = Some(*m);
        }
    }
    native
}

/// xh_panel_connector: whether a DRM connector of this type (DRM_MODE_CONNECTOR_*) can be the built-in panel: LVDS,
/// eDP, DSI, DPI. A phone's Virtual connectors (the writeback ones) report "connected" with modes up to 5120x2560, and
/// DisplayPort is whatever monitor is plugged in.
pub fn panel_connector(kind: u32) -> bool {
    matches!(kind, 7 | 14 | 16 | 17)
}

/// The display helper's report on display.sock: { "XHDP", width, height, hz, 0 } (u32s in the host's order).
pub const PANEL_REPORT_SIZE: usize = 20;
/// xh_panel_report: the panel a report states; None when it is not a usable one.
pub fn panel_report(message: &[u8]) -> Option<PanelMode> {
    if message.len() != PANEL_REPORT_SIZE || &message[..4] != b"XHDP" {
        return None;
    }
    let word = |i: usize| u32::from_ne_bytes(message[4 + 4 * i..8 + 4 * i].try_into().expect("4 bytes"));
    let (width, height, hz) = (word(0), word(1), word(2));
    (width != 0 && height != 0 && width <= 16384 && height <= 16384).then_some(PanelMode { width, height, hz })
}

// ---- RAM arithmetic and /proc reads (portable, unit-tested) -------------------------------------------------------

/// The share and what the floor leaves of `total`.
fn share_and_left(total: u64, percent: u32, floor: u64) -> (u64, u64) {
    let p = percent.min(100) as u64;
    (total / 100 * p + total % 100 * p / 100, total.saturating_sub(floor))
}

/// xh_ram_auto, ram=auto (docs/booter.md 1): min(percent % of total, total - floor), rounded down to 256 MiB, at most
/// RAM_MAX; 0 when that leaves the guest less than 1 GiB.
pub fn ram_auto(total: u64, percent: u32, floor: u64) -> u64 {
    let (share, left) = share_and_left(total, percent, floor);
    let ram = (share.min(left) & !(STEP_256M - 1)).min(RAM_MAX);
    if ram >= GIB {
        ram
    } else {
        0
    }
}

/// What the session needs outside the guest's RAM at its peak (ram_fit's terms, docs/booter.md 2.2, "What fits").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FitTerms {
    /// ram-session-reserve: MemAvailable the host keeps at the peak
    pub reserve: u64,
    /// ram-gpu-reserve: KGSL's own memory and the helpers'
    pub gpu: u64,
    /// ram-tasks and ram-task-cost: the iOS tasks' page tables and kernel memory
    pub tasks: u32,
    pub task_cost: u64,
    /// ram-tasks-awake and ram-task-idle-cost (task-trim): of the tasks, at most `awake` cost task_cost, the rest
    /// idle_cost each (trimmed); `awake` >= tasks: all of them task_cost (task-trim=0)
    pub awake: u32,
    pub idle_cost: u64,
    /// ram-hold-percent and ram-pagetable-percent: what scales with the guest's size, % of it
    pub hold_percent: u32,
    pub pagetable_percent: u32,
}

impl FitTerms {
    /// What does not scale with the guest's size: the headroom, the GPU's and the tasks' (each term < 1 TiB, at most
    /// 4096 tasks: no overflow).
    pub fn fixed(&self) -> u64 {
        self.reserve + self.gpu + self.tasks_bytes()
    }

    /// The tasks' term: `awake` of them (at most all) at task_cost, the others trimmed at idle_cost.
    pub fn tasks_bytes(&self) -> u64 {
        let awake = self.awake.min(self.tasks) as u64;
        awake * self.task_cost + (self.tasks as u64 - awake) * self.idle_cost
    }

    /// The share of the guest's size the host needs beside it, %.
    pub fn percent(&self) -> u64 {
        self.hold_percent as u64 + self.pagetable_percent as u64
    }

    /// MemAvailable left at the session's peak for a guest of `ram` from `available` at the start: available less the
    /// guest, its share, the tasks and the GPU (the headroom is what this must stay above); negative when it does not
    /// fit at all.
    pub fn left_at_peak(&self, available: u64, ram: u64) -> i64 {
        let need = ram + ram * self.percent() / 100 + self.gpu + self.tasks_bytes();
        available as i64 - need as i64
    }
}

impl Config {
    /// The value of a switch of xnuhost.conf (a catalog row with Place::Conf) as the switches line writes it; None for
    /// a name that is not one (`config::tests::the_catalog` checks every row has one).
    pub fn switch_value(&self, name: &str) -> Option<String> {
        let on = |b: bool| if b { "1" } else { "0" }.to_string();
        Some(match name {
            "session-mode" => self.session_mode.as_str().to_string(),
            "takeover-stops" if self.takeover_stops.is_empty() => "default".to_string(),
            "takeover-stops" => self.takeover_stops.clone(),
            "stop-loop" => self.stop_loop.as_str().to_string(),
            "takeover-reclaim" => on(self.takeover_reclaim),
            "takeover-swappiness" => self.takeover_swappiness.map_or("keep".to_string(), |n| n.to_string()),
            "task-trim" => on(self.task_trim),
            "timer-coalesce" => on(self.timer_coalesce),
            "ram-fit-terms" => (if self.ram_fit_terms_old { "old" } else { "tasks" }).to_string(),
            "ram-ondemand" => on(self.ram_ondemand),
            "uclamp-rule" => self.uclamp_rule.as_str().to_string(),
            "gpu-down" => self.gpu_down.as_str().to_string(),
            "gpu-kgsl-ui-rb" => on(self.gpu_kgsl_ui_rb),
            "gpu-off-level" => on(self.gpu_off_level),
            "gpu-blob-entries" => self.gpu_blob_entries.to_string(),
            "wifi-ring-wait" => on(self.wifi_ring_wait),
            "wifi-send-wait" => on(self.wifi_send_wait),
            "wifi-buffers" => on(self.wifi_buffers),
            "wifi-joined-scan" => self.wifi_joined_scan.as_str().to_string(),
            "encoder-10bit" => self.encoder_10bit.as_str().to_string(),
            "encoder-warmup" => on(self.encoder_warmup),
            "display-post" => (if self.display_post_serial { "serial" } else { "apart" }).to_string(),
            "display-present" => (if self.display_present_ahb { "ahb" } else { "window" }).to_string(),
            "decoder" => self.decoder.as_str().to_string(),
            "decoder-codecs" => oneencoder::switches::codecs_text(self.decoder_codecs),
            "astc" => on(self.astc),
            // sensors-als (2026-11-25): the sensor hub; the guest switches xnuhost adds are boot-args (the catalog's)
            "sensors" => self.sensors.as_str().to_string(),
            "sensors-hz" => self.sensors_hz.to_string(),
            "sensors-light" => self.sensors_light.map_or("auto".to_string(), |n| n.to_string()),
            _ => return None,
        })
    }

    /// The switches line (docs/switches.md), once at the start: every switch's value, and which are not at their
    /// default (main's behaviour). The switches, their order and their defaults are the catalog's
    /// (oneios_switches::SWITCHES, which oneios-tool's `switches` and the OneOS app's Switches screen read too). Kernel
    /// switches are boot-args: their value as boot-args gives it, else the kernel's (the catalog's default).
    pub fn switches_line(&self) -> String {
        let arg = |name: &str, default: &str| -> String {
            let key = format!("{name}=");
            self.boot_args
                .split_whitespace()
                .filter_map(|w| w.strip_prefix(key.as_str()))
                .next_back()
                .map_or(default.to_string(), str::to_string)
        };
        let all: Vec<(&str, String, &str)> = oneios_switches::SWITCHES
            .iter()
            .filter(|s| s.line)
            .map(|s| {
                let value = match s.place {
                    oneios_switches::Place::BootArg => arg(s.name, s.default),
                    oneios_switches::Place::Conf => self.switch_value(s.name).unwrap_or_else(|| "?".to_string()),
                };
                (s.name, value, s.default)
            })
            .collect();
        let changed: Vec<&str> = all.iter().filter(|(_, v, d)| v != d).map(|(k, _, _)| *k).collect();
        let words: Vec<String> = all.iter().map(|(k, v, _)| format!("{k}={v}")).collect();
        format!(
            "switches: {} (docs/switches.md); {}",
            words.join(" "),
            if changed.is_empty() {
                "all at their defaults".to_string()
            } else {
                format!("not at the default: {}", changed.join(" "))
            }
        )
    }

    /// ram_fit's terms from this configuration.
    pub fn fit_terms(&self) -> FitTerms {
        FitTerms {
            reserve: self.ram_session_reserve,
            gpu: self.ram_gpu_reserve,
            tasks: self.ram_tasks,
            task_cost: self.ram_task_cost,
            // Without the trim every task keeps its host mappings: all of them at ram-task-cost.
            awake: if self.task_trim && self.tasks { self.ram_tasks_awake } else { u32::MAX },
            idle_cost: self.ram_task_idle_cost,
            hold_percent: self.ram_hold_percent,
            pagetable_percent: self.ram_pagetable_percent,
        }
    }
}

/// xh_ram_fit: the guest's RAM that fits beside what the session needs outside it (docs/booter.md 2.2, "What fits"):
/// the largest G with G + G * (hold + pagetable percent) / 100 + tasks * task_cost + gpu + reserve <= available
/// (MemAvailable at xnuhost's start, after the fork stopped One UI). Not rounded; 0 when nothing fits.
pub fn ram_fit(available: u64, t: &FitTerms) -> u64 {
    let fixed = t.fixed();
    if available <= fixed {
        return 0;
    }
    (available - fixed) * 100 / (100 + t.percent()) // MemAvailable < 2^57: no overflow
}

// ---- the host's pressure (docs/booter.md 2.2, "The host's pressure") -------------------------------------------------
/// What the pressure watch says (Pressure::step).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressureNote {
    /// An event starts: MemAvailable is under the headroom (`short`), the host swaps in at PRESSURE_SWAP_IN or more
    /// (`swapping`), or both. The caller writes the line with the host's biggest users.
    Start { short: bool, swapping: bool },
    /// It is over (PRESSURE_CALM_S calm): how long it lasted, the lowest MemAvailable and the swap-ins in all, bytes.
    Over { seconds: u64, lowest: u64, swapped_in: u64 },
}

/// The pressure watch's state: one line when an event starts and one when it is over, never one per look.
#[derive(Debug, Default, Clone)]
pub struct Pressure {
    since: Option<u64>,
    calm_since: Option<u64>,
    lowest: u64,
    swapped_in: u64,
}

impl Pressure {
    /// One look at `now` (seconds): MemAvailable, the host's swap-ins in bytes a second over the last `seconds`, and
    /// the headroom (ram-session-reserve).
    pub fn step(&mut self, now: u64, available: u64, swap_in: u64, seconds: u64, reserve: u64) -> Option<PressureNote> {
        let short = available < reserve;
        let swapping = swap_in >= PRESSURE_SWAP_IN;
        let Some(start) = self.since else {
            if !short && !swapping {
                return None;
            }
            *self = Pressure { since: Some(now), calm_since: None, lowest: available, swapped_in: swap_in * seconds };
            return Some(PressureNote::Start { short, swapping });
        };
        self.lowest = self.lowest.min(available);
        self.swapped_in += swap_in * seconds;
        if available < reserve + PRESSURE_CALM_MARGIN || swap_in >= PRESSURE_SWAP_IN / 4 {
            self.calm_since = None;
            return None;
        }
        let calm = *self.calm_since.get_or_insert(now);
        if now - calm < PRESSURE_CALM_S {
            return None;
        }
        let note = PressureNote::Over { seconds: now - start, lowest: self.lowest, swapped_in: self.swapped_in };
        *self = Pressure::default();
        Some(note)
    }

    pub fn active(&self) -> bool {
        self.since.is_some()
    }
}

/// The host's /proc/meminfo figures the pressure line splits (bytes).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HostMemory {
    pub total: u64,
    pub free: u64,
    pub cached: u64,
    pub shmem: u64,
    pub anon: u64,
    pub page_tables: u64,
    pub slab_unreclaimable: u64,
    pub slab_reclaimable: u64,
    pub kernel_stack: u64,
}

/// The keys of HostMemory, in its order, for one proc_values read (MemAvailable and the swap read beside them).
pub const HOST_MEMORY_KEYS: [&str; 12] = [
    "MemTotal",
    "MemFree",
    "Cached",
    "Shmem",
    "AnonPages",
    "PageTables",
    "SUnreclaim",
    "SReclaimable",
    "KernelStack",
    "MemAvailable",
    "SwapTotal",
    "SwapFree",
];

impl HostMemory {
    /// From HOST_MEMORY_KEYS' values (None counts 0).
    pub fn from_values(v: &[Option<u64>]) -> HostMemory {
        let g = |i: usize| v.get(i).copied().flatten().unwrap_or(0);
        HostMemory {
            total: g(0),
            free: g(1),
            cached: g(2),
            shmem: g(3),
            anon: g(4),
            page_tables: g(5),
            slab_unreclaimable: g(6),
            slab_reclaimable: g(7),
            kernel_stack: g(8),
        }
    }
}

/// The host's memory by user, largest first, none of 0 bytes: the guest's RAM in the host's RAM (`guest`), the page
/// tables, the kernel's unreclaimable slab (the iOS tasks' VMAs among it) and stacks, the processes' anonymous memory,
/// the page cache, other shmem, the reclaimable slab, zram's pool (`zram`, 0 unknown), KGSL's own (`kgsl`), and the
/// rest that /proc/meminfo does not name (drivers' pages, dma-bufs, vmalloc). The guest's RAM is shmem: it is taken out
/// of Shmem and Cached.
pub fn host_users(m: &HostMemory, guest: u64, kgsl: u64, zram: u64) -> Vec<(&'static str, u64)> {
    let other_shmem = m.shmem.saturating_sub(guest);
    let file = m.cached.saturating_sub(m.shmem);
    let mut users = vec![
        ("the guest's RAM", guest),
        ("page tables", m.page_tables),
        ("unreclaimable slab", m.slab_unreclaimable),
        ("kernel stacks", m.kernel_stack),
        ("anonymous", m.anon),
        ("page cache", file),
        ("other shmem", other_shmem),
        ("reclaimable slab", m.slab_reclaimable),
        ("zram's pool", zram),
        ("KGSL's own", kgsl),
    ];
    let named: u64 = users.iter().map(|u| u.1).sum();
    users.push(("the rest", m.total.saturating_sub(m.free).saturating_sub(named)));
    users.retain(|u| u.1 != 0);
    users.sort_by_key(|u| std::cmp::Reverse(u.1));
    users
}

/// "the guest's RAM 5632, page tables 788, ..." in MiB.
pub fn host_users_text(users: &[(&str, u64)]) -> String {
    users.iter().map(|(name, b)| format!("{name} {}", b >> 20)).collect::<Vec<_>>().join(", ")
}

/// What changed between two looks of host_users (and the free RAM, `free`), largest change first, changes of 1 MiB or
/// more, in MiB with their sign: "free -5900, the guest's RAM +5632, page cache -290, ..." (the hold line, memory.rs).
pub fn host_change_text(before: &[(&str, u64)], free_before: u64, after: &[(&str, u64)], free_after: u64) -> String {
    let get = |l: &[(&str, u64)], name: &str| l.iter().find(|u| u.0 == name).map_or(0, |u| u.1) as i64;
    let mut names: Vec<&str> = before.iter().chain(after).map(|u| u.0).collect();
    names.sort_unstable();
    names.dedup();
    let mut d: Vec<(&str, i64)> = names.into_iter().map(|n| (n, get(after, n) - get(before, n))).collect();
    d.push(("free", free_after as i64 - free_before as i64));
    d.retain(|x| x.1.unsigned_abs() >= 1 << 20);
    d.sort_by_key(|x| std::cmp::Reverse(x.1.unsigned_abs()));
    d.iter().map(|(n, b)| format!("{n} {:+}", b / (1 << 20))).collect::<Vec<_>>().join(", ")
}

/// xh_ram_fit_auto, ram=auto: ram_auto and, unless `fit` is None (ram-fit=0), no more than `fit`, in 256 MiB steps; 0
/// under 1 GiB. The bound says which of the three decided (None with 0).
pub fn ram_fit_auto(total: u64, percent: u32, floor: u64, fit: Option<u64>) -> (u64, Option<RamBound>) {
    let mut ram = ram_auto(total, percent, floor);
    let (share, left) = share_and_left(total, percent, floor);
    let mut why = if share <= left { RamBound::Share } else { RamBound::Floor };
    if let Some(fit) = fit.filter(|_| ram != 0) {
        // No more than the host can hold beside what the session needs outside the guest (ram_fit): nothing to swap.
        let fits = fit & !(STEP_256M - 1);
        if fits < ram {
            ram = fits;
            why = RamBound::Available;
        }
        if ram < GIB {
            ram = 0;
        }
    }
    (ram, (ram != 0).then_some(why))
}

/// xh_swap_note: the host swaps the guest's RAM: say so once past 64 MiB, then at each further 256 MiB. `next` (0 at
/// the start) is the level the next line waits for; true when a line is due now.
pub fn swap_note(swapped: u64, next: &mut u64) -> bool {
    if *next == 0 {
        *next = SWAP_NOTE_FIRST;
    }
    if swapped <= *next {
        return false;
    }
    // Said once over 64 MiB, then again at each further 256 MiB: the next level is the first one at or above it.
    *next = SWAP_NOTE_FIRST + (swapped - SWAP_NOTE_FIRST).div_ceil(SWAP_NOTE_STEP) * SWAP_NOTE_STEP;
    true
}

/// xh_meminfo: MemTotal and MemAvailable of a /proc/meminfo file, in bytes; None when the file has not both (or a
/// MemTotal of 0).
pub fn meminfo(path: impl AsRef<Path>) -> Option<(u64, u64)> {
    match proc_values(path, &["MemTotal", "MemAvailable"])[..] {
        [Some(total), Some(available)] if total != 0 => Some((total, available)),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
