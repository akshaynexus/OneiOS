// SPDX-License-Identifier: MIT
//! xnuhost's switches (docs/switches.md) and every other key of `xnuhost.conf`, each with the rule of its values: the
//! one list both sides use.
//!
//! - xnuhost's `switches:` line is made from [`SWITCHES`] (its order, its words, its defaults); its tests check that
//!   every key xnuhost's parser takes is here, that [`check`] takes no value the parser refuses, and, for a switch,
//!   that both take exactly the same values (`runtime/xnuhost/src/config/tests.rs`, `the_catalog`).
//! - oneios-tool's `switches` writes [`SWITCHES`], [`KEYS`] and [`KEY_PREFIXES`] as JSON (the OneOS app's Switches
//!   screen, and the runtime package's `lib/switches.json`); its `conf` commands read and change an image's
//!   `overlay/userland/xnuhost.conf` with [`check`] (tools/oneios-tool/README.md, "switches" and "conf").
//!
//! A new switch is a row here (and its key in xnuhost's parser, and its row in docs/switches.md), in one commit.

/// Where a switch is written: its own `key=value` line of xnuhost.conf, or a word of its `boot-args-add=` line (a
/// kernel or plug-in switch, read by the guest from the boot arguments).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Conf,
    BootArg,
}

impl Place {
    pub fn as_str(self) -> &'static str {
        match self {
            Place::Conf => "conf",
            Place::BootArg => "boot-arg",
        }
    }
    pub fn parse(text: &str) -> Option<Place> {
        [Place::Conf, Place::BootArg].into_iter().find(|p| p.as_str() == text)
    }
}

/// What a phone session has shown of the switch's default (docs/switches.md, the column "Phone").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phone {
    Tested,
    NotBooted,
    Broken,
}

impl Phone {
    pub fn as_str(self) -> &'static str {
        match self {
            Phone::Tested => "tested",
            Phone::NotBooted => "not booted",
            Phone::Broken => "broken",
        }
    }
}

/// The rule of a value, as written in the tables.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    /// conf: 1/yes/true, 0/no/false (xnuhost's flags); a boot-arg: 0 or 1
    Bool,
    /// one of these words
    Choice(&'static [&'static str]),
    /// a whole decimal number in min..=max (conf: strtol's leading blanks and sign; a boot-arg: digits only)
    Int { min: i64, max: i64 },
    /// a comma list of these items, at least one; `unique`: none twice; `trim`: blanks around an item allowed
    List { items: &'static [&'static str], unique: bool, trim: bool },
    /// a size (xh_parse_size: a number, 0x hex or 0 octal, with K, M or G after it) in min..=max bytes
    Size { min: u64, max: u64 },
    /// any text shorter than max_len bytes (0: any length)
    Text { max_len: usize },
}

/// One switch of docs/switches.md.
#[derive(Clone, Copy, Debug)]
pub struct Switch {
    pub name: &'static str,
    pub place: Place,
    pub kind: Kind,
    /// words taken besides the kind's (auto, keep, 0 for a range that starts above it)
    pub also: &'static [&'static str],
    /// Some(what): any one word (no blank, no '=') is taken too, and this says what it is
    pub word: Option<&'static str>,
    /// the value when the file does not name it (main's behaviour; for a boot-arg the kernel's or the plug-in's)
    pub default: &'static str,
    /// the value that turns the change back (the behaviour before it); None for an option (its default is "off")
    pub off: Option<&'static str>,
    pub group: &'static str,
    pub title: &'static str,
    pub help: &'static str,
    pub phone: Phone,
    pub warning: Option<&'static str>,
    /// who reads it: xnuhost, fork (the OneOS app's switch), kernel, metal (the Metal plug-in), camera, display
    pub part: &'static str,
    /// Some(what): with the switch not written, xnuhost may choose another value itself; this says when
    pub auto: Option<&'static str>,
    /// whether xnuhost's `switches:` line has it
    pub line: bool,
}

/// A key of xnuhost.conf that is not a switch (docs/booter.md section 1): checked, never offered as a switch.
#[derive(Clone, Copy, Debug)]
pub struct Key {
    pub name: &'static str,
    pub kind: Kind,
    pub also: &'static [&'static str],
    /// the key may be given more than once, each line adds one (text-fixup, the disks)
    pub many: bool,
}

const fn conf(name: &'static str, kind: Kind, default: &'static str, off: Option<&'static str>) -> Switch {
    Switch {
        name,
        place: Place::Conf,
        kind,
        also: &[],
        word: None,
        default,
        off,
        group: "",
        title: "",
        help: "",
        phone: Phone::NotBooted,
        warning: None,
        part: "xnuhost",
        auto: None,
        line: true,
    }
}

const fn arg(name: &'static str, kind: Kind, default: &'static str, off: Option<&'static str>) -> Switch {
    Switch { place: Place::BootArg, part: "kernel", ..conf(name, kind, default, off) }
}

/// A switch's group, title and help (one or two sentences: what it does, what off does).
const fn text(s: Switch, group: &'static str, title: &'static str, help: &'static str) -> Switch {
    Switch { group, title, help, ..s }
}

const BOOL: Kind = Kind::Bool;
const TIB: u64 = 1 << 40;

/// xnuhost's TAKEOVER_STOPPABLE (config.rs; a test checks they agree).
pub const TAKEOVER_STOPPABLE: [&str; 8] =
    ["cameraserver", "mediaextractor", "mediametrics", "media.tuner", "perfmond", "wificond", "netd", "media"];

/// The switches, in the order of xnuhost's `switches:` line (takeover-stops, which the line does not have, after
/// session-mode).
pub const SWITCHES: &[Switch] = &[
    Switch {
        phone: Phone::Tested,
        ..text(
            conf("session-mode", Kind::Choice(&["hosted", "takeover"]), "hosted", None),
            "Session",
            "Session mode",
            "hosted: iOS runs beside One UI, as always. takeover: the opt-in Android takeover, which stops some of \
             One UI's services for more RAM (only on a phone whose host table allows it).",
        )
    },
    Switch {
        also: &["default", "none"],
        phone: Phone::Tested,
        part: "fork",
        line: false,
        ..text(
            conf("takeover-stops", Kind::List { items: &TAKEOVER_STOPPABLE, unique: true, trim: false }, "default", None),
            "Session",
            "Takeover: services stopped",
            "Takeover only: the One UI services the switch stops (default: its own list; none: none; or names, \
             comma separated). They are started again on the way back.",
        )
    },
    Switch {
        phone: Phone::Tested,
        part: "fork",
        ..text(
            conf("stop-loop", Kind::Choice(&["wait", "poll"]), "wait", Some("poll")),
            "Session",
            "Stop loop waits",
            "wait: the app's stop loop sleeps on init's properties until something changes. poll: it looks once a \
             second, as before.",
        )
    },
    Switch {
        part: "fork",
        ..text(
            conf("takeover-reclaim", BOOL, "1", Some("0")),
            "Session",
            "Takeover: reclaim caches",
            "Takeover only: before iOS starts the switch drops the page caches and compacts zram, so more RAM is free \
             for iOS. Off: none of it.",
        )
    },
    Switch {
        also: &["keep"],
        part: "fork",
        ..text(
            conf("takeover-swappiness", Kind::Int { min: 0, max: 200 }, "keep", None),
            "Session",
            "Takeover: swappiness",
            "Takeover only: vm.swappiness for the session (One UI's is put back on the way back); keep: not touched.",
        )
    },
    text(
        conf("task-trim", BOOL, "1", Some("0")),
        "Memory",
        "Trim idle tasks",
        "An idle iOS task's host mappings are dropped, so ram=auto can give iOS more RAM. Off: no trim, every task \
         keeps its mappings (the sizing before).",
    ),
    text(
        conf("timer-coalesce", BOOL, "1", Some("0")),
        "CPU",
        "Coalesce timers",
        "A vCPU's host timer is armed again only when the guest's deadline comes earlier, which saves a system call \
         per timer change. Off: every new deadline and cancel is a timer_settime, as before.",
    ),
    text(
        conf("ram-fit-terms", Kind::Choice(&["tasks", "old"]), "tasks", Some("old")),
        "Memory",
        "RAM sizing terms",
        "tasks: ram=auto counts the iOS tasks and keeps 1536 MiB of headroom. old: the terms before 3b8742a (768 MiB \
         headroom, no task cost).",
    ),
    Switch {
        warning: Some(
            "Against the fixed-RAM rule: iOS's RAM is then locked page by page as iOS touches it, and an iOS that fills \
             it can end the session.",
        ),
        ..text(
            conf("ram-ondemand", BOOL, "0", None),
            "Memory",
            "RAM on demand",
            "An option, off by default: the guest's RAM is not filled in at the start; each page is locked when iOS \
             first touches it and free pages are given back.",
        )
    },
    text(
        conf("uclamp-rule", Kind::Choice(&["work", "all"]), "work", Some("all")),
        "CPU",
        "CPU clamp rule",
        "work: xnuhost alone sets each vCPU thread's clamp from its work (0 when idle); the GPU's animation boost goes \
         only to the frame's threads. all: the boost on every vCPU and iOS thread, as before.",
    ),
    text(
        conf("gpu-down", Kind::Choice(&["governor", "bottom"]), "governor", Some("bottom")),
        "GPU",
        "GPU clock comes down",
        "governor: the GPU leaves its top level when a frame fits at the governor's clock, which saves heat. bottom: \
         only when it would fit at the lowest clock (before: on the S24 Ultra it stayed at the top).",
    ),
    text(
        conf("gpu-kgsl-ui-rb", BOOL, "0", Some("1")),
        "GPU",
        "Skip Android UI GPU ringbuffer",
        "0: the render server and front app run at KGSL priority 8 (out of Android's 4-7 UI ringbuffer, avoiding tile-memory \
         collisions with One UI). 1: priority 4 (in the UI ringbuffer, as before).",
    ),
    text(
        conf("gpu-off-level", BOOL, "1", Some("0")),
        "GPU",
        "Lowest GPU level when iOS display is off",
        "1: hold KGSL at its lowest clock while iOS's display is off so standby wakes do not boost the GPU. 0: leave the \
         render server at the governor's choice (as before).",
    ),
    text(
        conf("gpu-blob-entries", Kind::Int { min: 16384, max: 65532 }, "65532", Some("16384")),
        "GPU",
        "GPU blob size limit",
        "The most entries of one guest 3D blob the GPU device takes. 16384: the limit before 78c4b86 (a large blob of \
         single pages was refused).",
    ),
    Switch {
        phone: Phone::Tested,
        ..text(
            conf("wifi-ring-wait", BOOL, "1", Some("0")),
            "Wi-Fi",
            "Wait for iOS's receive ring",
            "A frame for iOS that finds its receive ring full waits briefly for iOS. Off: it is dropped at once.",
        )
    },
    text(
        conf("wifi-send-wait", BOOL, "1", Some("0")),
        "Wi-Fi",
        "Wait for wlan0's send buffer",
        "A frame from iOS that finds wlan0's send buffer full waits up to 20 ms. Off: it fails at once.",
    ),
    Switch {
        phone: Phone::Tested,
        ..text(
            conf("wifi-buffers", BOOL, "1", Some("0")),
            "Wi-Fi",
            "Larger Wi-Fi buffers",
            "wlan0's packet socket gets a 4 MiB receive and a 1 MiB send buffer. Off: the kernel's defaults.",
        )
    },
    text(
        conf("wifi-joined-scan", Kind::Choice(&["cache", "low", "plain"]), "cache", Some("plain")),
        "Wi-Fi",
        "Scans while joined",
        "cache: a scan within 120 s of the last one is answered from it, else a low-priority scan. low: every scan \
         the radio's, low priority. plain: every scan the radio's, as before.",
    ),
    text(
        conf("encoder-10bit", Kind::Choice(&["main10", "8bit", "nv12"]), "main10", Some("nv12")),
        "Video",
        "10-bit video encoding",
        "main10: iOS's 10-bit frames are coded as HEVC Main10 where the encoder offers it. 8bit: taken but coded \
         8-bit. nv12: no 10-bit frames are offered to iOS.",
    ),
    text(
        conf("encoder-warmup", BOOL, "1", Some("0")),
        "Video",
        "Encoder warm-up",
        "The encoder helper starts at iOS's first video session and opens one encoder ahead. Off: it starts at the \
         first encode.",
    ),
    Switch {
        part: "display",
        ..text(
            conf("display-post", Kind::Choice(&["apart", "serial"]), "apart", Some("serial")),
            "Display",
            "Frames read while posting",
            "apart: the display helper reads and answers each frame while it waits to post the last one. serial: it \
             reads nothing until the last frame is posted, as before.",
        )
    },
    Switch {
        part: "display",
        ..text(
            conf("display-present", Kind::Choice(&["ahb", "window"]), "ahb", Some("window")),
            "Display",
            "Present in hardware buffers",
            "ahb: the display helper posts frames in write-only hardware buffers, with no CPU cache work per frame \
             (falls back by itself where the phone lacks them). window: through the layer's window, as before.",
        )
    },
    text(
        conf("decoder", Kind::Choice(&["auto", "off"]), "auto", Some("off")),
        "Video",
        "Hardware video decoding",
        "auto: iOS's video decoder gets the phone's MediaCodec for H.264, HEVC, VP9 and AV1 (needs encoder=auto). off: \
         iOS decodes video in software, as before.",
    ),
    text(
        conf("decoder-codecs", Kind::List { items: &["h264", "hevc", "vp9", "av1"], unique: false, trim: true }, "h264,hevc,vp9,av1", None),
        "Video",
        "Decoded codecs",
        "The codecs the phone decodes for iOS, comma separated; one left out is decoded in software by iOS.",
    ),
    text(
        conf("astc", BOOL, "1", Some("0")),
        "GPU",
        "ASTC textures on the GPU",
        "iOS hands ASTC images to the GPU instead of decoding them on the CPU (much faster scrolling). Off: decoded \
         on the CPU, as before.",
    ),
    text(
        conf("sensors", Kind::Choice(&["off", "auto", "ssc"]), "auto", Some("off")),
        "Sensors",
        "Sensor hub",
        "auto: xnuhost reads the phone's sensor hub for iOS's motion sensors and front light sensor (the old path when \
         the hub does not answer). ssc: the same, but no rear light sensor. off: neither, as before.",
    ),
    text(
        conf("sensors-hz", Kind::Int { min: 5, max: 200 }, "50", None),
        "Sensors",
        "Motion sensor rate",
        "The accelerometer's and gyroscope's rate on the hub, in Hz (the magnetometer at most 25).",
    ),
    Switch {
        also: &["auto"],
        ..text(
            conf("sensors-light", Kind::Int { min: 0, max: 7 }, "auto", None),
            "Sensors",
            "Which light sensor",
            "Which of the hub's light sensors: auto takes the first that is not a rear one; N takes that one.",
        )
    },
    Switch {
        auto: Some(
            "xnuhost adds 7 (15 with a magnetometer) with sensors=auto or ssc when the hub has an accelerometer and the \
             kernel collection has no com.apple.driver.AppleSPU",
        ),
        warning: Some(
            "With a kernel collection prelinked before fix/motion-spu-clash (it still has com.apple.driver.AppleSPU) a \
             non-zero value panicked v0.1.29 at the kernel's start; xnuhost itself gives no motion device then.",
        ),
        ..text(
            arg("oneios-motion", Kind::Int { min: 0, max: 15 }, "0", Some("0")),
            "Sensors",
            "Motion sensors for iOS",
            "Bits of the motion devices published to iOS (1 accelerometer, 2 gyroscope, 4 orientation, 8 compass): \
             rotation, the compass and CoreMotion. 0: none published, as before.",
        )
    },
    Switch {
        auto: Some("xnuhost adds oneios-als-spu=1 when the light comes from the sensor hub (sensors=auto)"),
        ..text(
            arg("oneios-als-spu", BOOL, "0", Some("0")),
            "Sensors",
            "Light sensor as an iPhone's",
            "1: the light sensor is published as an iPhone's own, so auto-brightness works as on an iPhone. 0: the \
             generic HID light sensor, as before.",
        )
    },
    text(
        arg("oneios-wifi-rssi", BOOL, "1", Some("0")),
        "Wi-Fi",
        "Wi-Fi bars follow the signal",
        "The Wi-Fi driver reports signal changes, so the bars follow the signal. Off: the signal is read at the join \
         only.",
    ),
    text(
        arg("oneios-telemetry-jobs", BOOL, "0", Some("1")),
        "iOS",
        "Telemetry jobs",
        "0: iOS's telemetry uploaders (osanalyticshelper and 16 more) are disabled. 1: they run, as before.",
    ),
    Switch {
        part: "metal",
        ..text(
            arg("oneios-pool-cut", BOOL, "1", Some("0")),
            "GPU",
            "Release spare command pools",
            "The Metal plug-in releases command pools larger than their recent need. Off: pools are kept (624bab3 \
             add-on only).",
        )
    },
    Switch {
        also: &["0"],
        ..text(
            arg("oneios-pressure-big", Kind::Int { min: 1, max: 65536 }, "512", Some("0")),
            "Memory",
            "Memory pressure: big first",
            "At a memory pressure rise, processes of at least this many MiB are told first, the largest first (the \
             render server hears it early). 0: XNU's own order.",
        )
    },
    Switch {
        word: Some("a product type (iPhone18,2): the jetsam plist of that product"),
        ..text(
            arg("oneios-disable-allowed", BOOL, "1", Some("0")),
            "iOS",
            "launchd honours disabled jobs",
            "The kernel lets launchd honour disabled.plist for every job, so disabled daemons stay off. 0: the jobs \
             marked in Apple's plist run whatever disabled.plist says.",
        )
    },
    text(
        arg("oneios-nested-exact", BOOL, "1", Some("0")),
        "Memory",
        "Exact shared-region flushes",
        "A shared-region flush goes only to the tasks that have a page of its range. Off: to every task that filled \
         any page of the block.",
    ),
    text(
        arg("oneios-upgrade-flush", BOOL, "0", Some("1")),
        "Memory",
        "Flush on same-page upgrades",
        "0: a page entry changed to the same page with no less access keeps the tasks' mappings. 1: such a change \
         unmaps it everywhere, as before.",
    ),
    text(
        arg("oneios-fill-fast-fault", BOOL, "1", Some("0")),
        "Memory",
        "Fast faults on the fill path",
        "A user fault that waits for its reference or modified bit is resolved on the fill path. Off: through the \
         exception path, as before.",
    ),
    text(
        arg("oneios-audio-rt", BOOL, "1", Some("0")),
        "Audio",
        "Audio threads on time",
        "iOS's audio engine threads are time-constraint threads of iOS's scheduler (5 ms periods, no timer slop). \
         Off: priority 81, as before (late periods).",
    ),
    text(
        arg("oneios-audio-curve", BOOL, "1", Some("0")),
        "Audio",
        "Louder volume curve",
        "The speaker's volume steps follow a -36..0 dB curve, so half volume is about -9 dB. Off: -64..0 dB in even \
         steps (quiet), as before.",
    ),
    text(
        arg("oneios-codec-irq", BOOL, "1", Some("0")),
        "Video",
        "Video codec on interrupts",
        "The video codec's transport sleeps until the media device's interrupt. Off: it polls every 1 ms while \
         streaming.",
    ),
    Switch {
        part: "metal",
        ..text(
            arg("oneios-idle-spares", BOOL, "1", Some("0")),
            "GPU",
            "Free idle GPU spares",
            "The Metal plug-in's idle trim also frees parked images, buffers and spare image blocks. Off: they age only \
             as before (624bab3 add-on only).",
        )
    },
    Switch {
        part: "metal",
        ..text(
            arg("oneios-bo-cache-idle", BOOL, "1", Some("0")),
            "GPU",
            "Trim the GPU buffer cache",
            "Every 60 s the plug-in gives back cached GPU buffers no allocation took for 30 s. Off: the cache shrinks \
             only past its budgets.",
        )
    },
    Switch {
        part: "metal",
        ..text(
            arg("oneios-keep-room", Kind::Int { min: 0, max: 1024 }, "8", Some("0")),
            "GPU",
            "GPU pool room kept (MiB)",
            "metal2vulkan keeps at most this many MiB free in each pool for coming placements, so pools grow less. 0: \
             the runtime's half block, as before (624bab3 add-on only).",
        )
    },
    Switch {
        part: "metal",
        ..text(
            arg("oneios-dl-full", BOOL, "1", Some("0")),
            "Display",
            "Animations at the panel's rate",
            "UIKit's in-process animation links run at the panel's rate instead of the 30 Hz they ask for. Off: every \
             rate as asked.",
        )
    },
    Switch {
        also: &["0"],
        ..text(
            arg("sheng-vsync-min", Kind::Int { min: 24, max: 120 }, "60", Some("24")),
            "Display",
            "Lowest refresh rate (Hz)",
            "With sheng-vsync-fixed=0, 60 reports a 60..120 Hz range and 24 (or 0) restores 24..120 Hz. The fixed-rate \
             default above 60 Hz overrides this range unless explicitly set to 0.",
        )
    },
    Switch {
        auto: Some("0 (since 2026-11-28; xnuhost added 1 above 60 Hz before): the 60..120 Hz range of sheng-vsync-min"),
        ..text(
            arg("sheng-vsync-fixed", BOOL, "0", Some("1")),
            "Display",
            "Keep animations at panel rate",
            "0: the variable range (sheng-vsync-min..the panel's rate, 60..120 Hz): 120 Hz for touch and scrolling, 60 for \
             light animations, cooler. 1: one fixed panel rate, every animation at 120 Hz.",
        )
    },
    Switch {
        also: &["0"],
        part: "metal",
        ..text(
            arg("oneios-compile-s", Kind::Int { min: 1, max: 600 }, "120", Some("0")),
            "GPU",
            "Shader compile deadline (s)",
            "Every Metal client but the render server gives a pipeline compile this many seconds (the camera's kernels \
             took longer than 20). 0: 20 s everywhere, as before.",
        )
    },
    Switch {
        part: "metal",
        ..text(
            arg("oneios-snapshot-astc", BOOL, "0", Some("1")),
            "iOS",
            "App snapshots as ASTC",
            "0: SpringBoard's app snapshots are JPEG or PNG, never ASTC, which the CPU would take seconds to make. 1: \
             iOS's own choice.",
        )
    },
    Switch {
        part: "camera",
        ..text(
            arg("oneios-camera-notify", BOOL, "1", Some("0")),
            "Camera",
            "Camera stream notifications",
            "The capture plug-in posts the stream's start and stop notifications as an iPhone's camera does. Off: none \
             posted, as before.",
        )
    },
    Switch {
        part: "metal",
        ..text(
            arg("oneios-m2v-shader-fix", BOOL, "1", Some("0")),
            "GPU",
            "Adreno shader fixes",
            "The Metal translator's two Adreno shader fixes (large constant tables, pointer constants). Off: those \
             shaders are translated as before; the first boot after a change translates every shader again.",
        )
    },
    Switch {
        part: "metal",
        ..text(
            arg("oneios-dont-care", BOOL, "0", Some("1")),
            "GPU",
            "DontCare stores (rt add-on)",
            "rt add-on only: 0: Metal's DontCare stores store, so a render pass the runtime splits keeps what it drew. \
             1: they are Vulkan DONT_CARE on Turnip, a faster pass, but a split pass loses its first draws (garbage \
             on screen); for an A/B only.",
        )
    },
    Switch {
        part: "metal",
        ..text(
            arg("oneios-queue-priority", BOOL, "1", Some("0")),
            "GPU",
            "Render queues high priority (rt)",
            "rt add-on only: the render server's GPU queues at the high priority. Off: the driver's default.",
        )
    },
];

const fn key(name: &'static str, kind: Kind) -> Key {
    Key { name, kind, also: &[], many: false }
}
const fn path(name: &'static str) -> Key {
    key(name, Kind::Text { max_len: 512 })
}
const fn flag(name: &'static str) -> Key {
    key(name, Kind::Bool)
}
const fn int(name: &'static str, min: i64, max: i64) -> Key {
    key(name, Kind::Int { min, max })
}
const fn sized(name: &'static str) -> Key {
    key(name, Kind::Size { min: 0, max: TIB - 1 })
}
/// atoi's keys: any text is a number to xnuhost (0 without digits)
const fn atoi(name: &'static str) -> Key {
    key(name, Kind::Text { max_len: 0 })
}

/// The keys of xnuhost.conf that are not switches: xnuhost's parser (config.rs `apply`) takes each with this rule (a
/// test checks it takes all of them, and that none of these rules takes a value the parser refuses; the rule may be
/// stricter, as for a number atoi would read from "8x").
pub const KEYS: &[Key] = &[
    atoi("session-abi"),
    key("mode", Kind::Text { max_len: 16 }),
    path("runtime"),
    path("image"),
    path("data"),
    path("session"),
    path("kernel"),
    path("identity"),
    key("boot-args", Kind::Text { max_len: 1024 }),
    path("boot-args-add"),
    key("display", Kind::Text { max_len: 16 }),
    key("display-format", Kind::Choice(&["bgrx", "rgba16f"])),
    key("gpu", Kind::Choice(&["auto", "3d", "2d"])),
    key("gpu-level", Kind::Choice(&["deadline", "hold", "governor"])),
    key("gpu-node", Kind::Text { max_len: 128 }),
    int("gpu-priority", 0, 1),
    int("cpus", 1, 8),
    // (xnuhost takes up to 2^64 - 1; JSON readers that keep a long stop at 2^63 - 1)
    Key { also: &["auto"], ..key("ram", Kind::Size { min: 1, max: i64::MAX as u64 }) },
    Key { also: &["off", "auto"], ..sized("ram-view") },
    int("ram-percent", 10, 100),
    sized("ram-host-floor"),
    key("ram-meminfo", Kind::Text { max_len: 256 }),
    int("host-mem-log", 0, 86400),
    int("usb-log", 0, 3600),
    flag("ram-giveback"),
    flag("ram-prefault"),
    flag("ram-lock"),
    sized("ram-session-reserve"),
    flag("ram-fit"),
    flag("ram-huge"),
    sized("ram-gpu-reserve"),
    int("ram-pagetable-percent", 0, 50),
    int("ram-hold-percent", 0, 50),
    int("ram-tasks", 0, 4096),
    sized("ram-task-cost"),
    sized("ram-unlock-below"),
    int("return-graceful-ms", 0, 120000),
    int("ready-content-wait", 0, 480),
    atoi("width"),
    atoi("height"),
    Key { also: &["0"], ..int("hz", 24, 240) },
    key("panel-fallback", Kind::Text { max_len: 0 }),
    key("scale", Kind::Choice(&["2", "3"])),
    flag("scale-fixup"),
    int("corner-radius", 0, 200),
    int("artwork-subtype", 0, 10000),
    flag("ios16-fixup"),
    Key { many: true, ..key("text-fixup", Kind::Text { max_len: 160 }) },
    flag("input"),
    flag("display-direct"),
    flag("touch-exclusive"),
    path("input-script"),
    flag("tasks"),
    atoi("task-uid"),
    flag("touch-sleep"),
    key("pen", Kind::Choice(&["off", "auto"])),
    key("pen-rotate", Kind::Choice(&["auto", "0", "90", "180", "270"])),
    flag("pen-mirror"),
    key("encoder", Kind::Choice(&["off", "auto", "sim"])),
    key("camera-vm", Kind::Choice(&["hide", "show"])),
    key("camera-model", Kind::Text { max_len: 16 }),
    flag("bridges"),
    key("clusters", Kind::Text { max_len: 9 }),
    key("wifi", Kind::Choice(&["auto", "off", "tap", "cell", "sim"])),
    key("bluetooth", Kind::Choice(&["auto", "virtual", "hidl", "sim"])),
    key("haptics", Kind::Choice(&["auto", "off", "sim"])),
    flag("haptics-native-impacts"),
    flag("legacy-boot-args"),
    flag("setway-skip"),
    flag("blk-late-flush"),
    int("perf-log", 0, 86400),
    int("spin-host", 0, 1000),
    int("spin-task", 0, 1000),
    Key { also: &["2"], ..flag("cpu-pin") },
    flag("map-defer"),
    int("task-trim-idle", 1, 86400),
    flag("thermal-cap"),
    int("ram-tasks-awake", 0, 4096),
    sized("ram-task-idle-cost"),
    key("host-device", Kind::Text { max_len: 32 }),
    key("host-props", Kind::Text { max_len: 256 }),
    key("host-root", Kind::Text { max_len: 256 }),
    atoi("log-level"),
    Key { many: true, ..key("disk", Kind::Text { max_len: 0 }) },
    Key { many: true, ..key("disk-rw", Kind::Text { max_len: 0 }) },
    Key { many: true, ..key("data-store", Kind::Text { max_len: 0 }) },
];

/// Keys taken by their start (hw-backend, hw-backend-BRIDGE, hw-backend-fallback: onebridges::hw), any text.
pub const KEY_PREFIXES: &[&str] = &["hw-backend"];

/// The switch of that name.
pub fn switch(name: &str) -> Option<&'static Switch> {
    SWITCHES.iter().find(|s| s.name == name)
}

/// The key (not a switch) of that name.
pub fn key_named(name: &str) -> Option<&'static Key> {
    KEYS.iter().find(|k| k.name == name)
}

// ---- the rules, owned (the catalog may also come from a runtime's lib/switches.json) -----------------------------

/// A rule's kind, as the JSON names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleKind {
    Bool,
    Choice,
    Int,
    List,
    Size,
    Text,
}

/// A value's rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub kind: RuleKind,
    pub place: Place,
    /// Choice: the words; List: the items
    pub values: Vec<String>,
    /// Int and Size: the range
    pub min: i128,
    pub max: i128,
    pub also: Vec<String>,
    /// any one word is taken too
    pub word: bool,
    /// List
    pub unique: bool,
    pub trim: bool,
    /// Text: shorter than this many bytes (0: any)
    pub max_len: usize,
}

impl Rule {
    pub fn new(kind: Kind, place: Place, also: &[&str], word: bool) -> Rule {
        let mut rule = Rule {
            kind: RuleKind::Text,
            place,
            values: Vec::new(),
            min: 0,
            max: 0,
            also: also.iter().map(|s| s.to_string()).collect(),
            word,
            unique: false,
            trim: false,
            max_len: 0,
        };
        let own = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match kind {
            Kind::Bool => rule.kind = RuleKind::Bool,
            Kind::Choice(list) => {
                rule.kind = RuleKind::Choice;
                rule.values = own(list);
            }
            Kind::Int { min, max } => {
                rule.kind = RuleKind::Int;
                (rule.min, rule.max) = (min.into(), max.into());
            }
            Kind::List { items, unique, trim } => {
                rule.kind = RuleKind::List;
                rule.values = own(items);
                (rule.unique, rule.trim) = (unique, trim);
            }
            Kind::Size { min, max } => {
                rule.kind = RuleKind::Size;
                (rule.min, rule.max) = (min.into(), max.into());
            }
            Kind::Text { max_len } => rule.max_len = max_len,
        }
        rule
    }

    /// The values it takes, in words (for a refusal).
    pub fn describe(&self) -> String {
        let mut text = match self.kind {
            RuleKind::Bool if self.place == Place::BootArg => "0 or 1".to_string(),
            RuleKind::Bool => "1, yes, true, 0, no or false".to_string(),
            RuleKind::Choice => format!("one of {}", self.values.join(", ")),
            RuleKind::Int => format!("a whole number from {} to {}", self.min, self.max),
            RuleKind::List => {
                format!("{} (comma separated{})", self.values.join(", "), if self.unique { ", each once" } else { "" })
            }
            RuleKind::Size => {
                format!("a size from {} to {} bytes (a number with K, M or G after it)", self.min, self.max)
            }
            RuleKind::Text if self.max_len > 0 => format!("text shorter than {} bytes", self.max_len),
            RuleKind::Text => "any text".to_string(),
        };
        if !self.also.is_empty() {
            text = format!("{text}, or {}", self.also.join(", "));
        }
        if self.word {
            text.push_str(", or one word");
        }
        text
    }
}

impl Switch {
    pub fn rule(&self) -> Rule {
        Rule::new(self.kind, self.place, self.also, self.word.is_some())
    }
}

impl Key {
    pub fn rule(&self) -> Rule {
        Rule::new(self.kind, Place::Conf, self.also, false)
    }
}

/// C's isspace: what xnuhost trims from a line and strtol skips.
pub const C_SPACE: [char; 6] = [' ', '\t', '\n', '\r', '\x0b', '\x0c'];

/// A boot argument's word: not empty, no blank, no '=' (what the kernel reads as one value).
pub fn is_word(value: &str) -> bool {
    !value.is_empty() && value.len() < 128 && !value.contains(|c: char| c.is_whitespace() || c == '=')
}

/// Whether `value` follows `rule`; Err says what it takes.
pub fn check(rule: &Rule, value: &str) -> Result<(), String> {
    let refused = || Err(rule.describe());
    if rule.place == Place::BootArg && !is_word(value) {
        return Err(format!("{} (one word, no blank)", rule.describe()));
    }
    if rule.also.iter().any(|a| a == value) || (rule.word && is_word(value)) {
        return Ok(());
    }
    let ok = match rule.kind {
        RuleKind::Bool if rule.place == Place::BootArg => matches!(value, "0" | "1"),
        RuleKind::Bool => matches!(value, "1" | "yes" | "true" | "0" | "no" | "false"),
        RuleKind::Choice => rule.values.iter().any(|v| v == value),
        RuleKind::Int => {
            let n = if rule.place == Place::BootArg {
                value.bytes().all(|b| b.is_ascii_digit()).then(|| value.parse::<i128>().ok()).flatten()
            } else {
                strtol_whole(value).map(i128::from)
            };
            n.is_some_and(|n| (rule.min..=rule.max).contains(&n))
        }
        RuleKind::List => {
            let mut seen: Vec<&str> = Vec::new();
            value.split(',').all(|item| {
                let item = if rule.trim { item.trim() } else { item };
                let fresh = !(rule.unique && seen.contains(&item));
                seen.push(item);
                fresh && rule.values.iter().any(|v| v == item)
            })
        }
        RuleKind::Size => size(value).is_some_and(|n| (rule.min..=rule.max).contains(&i128::from(n))),
        RuleKind::Text => rule.max_len == 0 || value.len() < rule.max_len,
    };
    if ok {
        Ok(())
    } else {
        refused()
    }
}

// ---- numbers as xnuhost reads them (xh_core::parse; a test of xnuhost compares the two) ------------------------

/// strtol(value, 10) with nothing after the number: leading blanks and one sign allowed; a value past 64 bits
/// saturates (and so falls out of any range).
pub fn strtol_whole(value: &str) -> Option<i64> {
    let t = value.trim_start_matches(C_SPACE);
    let (neg, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v = t.parse::<u64>().ok();
    Some(match (neg, v) {
        (false, v) => v.and_then(|v| i64::try_from(v).ok()).unwrap_or(i64::MAX),
        (true, v) => v.and_then(|v| 0i64.checked_sub_unsigned(v)).unwrap_or(i64::MIN),
    })
}

/// xh_parse_size: strtoull(value, 0) (leading blanks and '+', no '-'; 0x hex, a leading 0 octal), then K, M or G,
/// nothing after; None when it overflows.
pub fn size(value: &str) -> Option<u64> {
    let t = value.trim_start_matches(C_SPACE);
    if t.starts_with('-') {
        return None;
    }
    let t = t.strip_prefix('+').unwrap_or(t);
    let (radix, digits) = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) if hex.starts_with(|c: char| c.is_ascii_hexdigit()) => (16, hex),
        _ if t.len() > 1 && t.starts_with('0') && !t.starts_with("0x") && !t.starts_with("0X") => (8, &t[1..]),
        _ => (10, t),
    };
    let end = digits.find(|c: char| !c.is_digit(radix)).unwrap_or(digits.len());
    let (v, rest) = if end == 0 {
        if radix != 8 {
            return None;
        }
        (0, digits)
    } else {
        (u64::from_str_radix(&digits[..end], radix).ok()?, &digits[end..])
    };
    let (shift, rest) = match rest.as_bytes().first() {
        Some(b'k' | b'K') => (10, &rest[1..]),
        Some(b'm' | b'M') => (20, &rest[1..]),
        Some(b'g' | b'G') => (30, &rest[1..]),
        _ => (0, rest),
    };
    let bytes = v.checked_mul(1u64 << shift)?;
    rest.is_empty().then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_switch_is_whole() {
        for (i, s) in SWITCHES.iter().enumerate() {
            assert!(!s.group.is_empty() && !s.title.is_empty() && !s.help.is_empty(), "{}", s.name);
            assert!(s.title.len() <= 40, "{}: title of {} bytes", s.name, s.title.len());
            assert!(!SWITCHES[..i].iter().any(|o| o.name == s.name), "{} twice", s.name);
            assert!(key_named(s.name).is_none(), "{} is a switch and a key", s.name);
            let rule = s.rule();
            assert_eq!(check(&rule, s.default), Ok(()), "{}: its default", s.name);
            if let Some(off) = s.off {
                assert_eq!(check(&rule, off), Ok(()), "{}: its off", s.name);
            }
        }
        for (i, k) in KEYS.iter().enumerate() {
            assert!(!KEYS[..i].iter().any(|o| o.name == k.name), "{} twice", k.name);
        }
    }

    #[test]
    fn rules() {
        let b = switch("oneios-pool-cut").unwrap().rule();
        assert!(check(&b, "0").is_ok() && check(&b, "1").is_ok());
        assert_eq!(check(&b, "yes"), Err("0 or 1".into()));
        assert!(check(&b, "1 x").is_err() && check(&b, "").is_err());
        let c = switch("wifi-buffers").unwrap().rule();
        assert!(["0", "1", "yes", "no", "true", "false"].iter().all(|v| check(&c, v).is_ok()));
        assert!(check(&c, "on").is_err());
        let i = switch("sensors-light").unwrap().rule();
        assert!(check(&i, "auto").is_ok() && check(&i, "7").is_ok() && check(&i, " +3").is_ok());
        assert!(check(&i, "8").is_err() && check(&i, "-1").is_err() && check(&i, "3x").is_err());
        let v = switch("sheng-vsync-min").unwrap().rule();
        assert!(check(&v, "0").is_ok() && check(&v, "24").is_ok() && check(&v, "120").is_ok());
        assert!(check(&v, "23").is_err() && check(&v, "+60").is_err());
        let l = switch("takeover-stops").unwrap().rule();
        assert!(check(&l, "default").is_ok() && check(&l, "netd,media").is_ok());
        assert!(check(&l, "netd,netd").is_err() && check(&l, "netd, media").is_err() && check(&l, "").is_err());
        let d = switch("decoder-codecs").unwrap().rule();
        assert!(check(&d, "vp9, av1").is_ok() && check(&d, "h264,h264").is_ok());
        assert!(check(&d, "vp8").is_err() && check(&d, "").is_err() && check(&d, "h264,").is_err());
        let w = switch("oneios-disable-allowed").unwrap().rule();
        assert!(check(&w, "iPhone18,2").is_ok() && check(&w, "0").is_ok());
        assert!(check(&w, "a b").is_err());
    }

    #[test]
    fn numbers() {
        assert_eq!(strtol_whole(" -12"), Some(-12));
        assert_eq!(strtol_whole("99999999999999999999"), Some(i64::MAX));
        assert_eq!(strtol_whole("1 "), None);
        assert_eq!(size("512M"), Some(512 << 20));
        assert_eq!(size("0x10k"), Some(16 << 10));
        assert_eq!(size("010"), Some(8));
        assert_eq!(size("0"), Some(0));
        assert_eq!(size("08"), None);
        assert_eq!(size("-1"), None);
        assert_eq!(size("1T"), None);
        assert_eq!(size("99999999999G"), None);
    }
}
