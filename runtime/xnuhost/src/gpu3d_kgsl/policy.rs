// SPDX-License-Identifier: MIT
//! OneiOS: the GPU's per-process policy, as pure functions with no KGSL call (unit-tested on any host). The port of
//! drm2kgsl/oneios_gpu_policy.h; the patched renderer's C still includes that header for the two parts it uses itself
//! (og_class_name, og_kgsl_priority), and a test checks they say what [`Class::name`] and [`kgsl_priority`] say. Three
//! parts (docs/gpu.md 2.2b):
//!
//! 1. A context's CLASS, from its owner: the render server, the front app, the default, or a background process. The
//!    class gives the KGSL priority of the context's queues (DRAWCTXT_CREATE's priority bits, fixed when a queue is
//!    made) and decides whether its frames count for the GPU's level. The sources, in order:
//!    - the name "xnu:backboardd" (the render server; the name the guest's DroidVMVirtioGpu3D.cpp gives a context);
//!    - whether the render server reads the context's surfaces ([`Pages`] below: a blob of the render server over the
//!      same guest pages as one of the context's). A context the user sees is never background, whatever else says so;
//!    - process names ([`DEFAULT_BACKGROUND`], [`DEFAULT_FRONT`], or ONEIOS_GPU_BACKGROUND/_FRONT): only processes that
//!      never draw what the user sees (each named below with the reason);
//!    - the guest's hint: the submitting task's effective role (TASK_POLICY_ROLE, RunningBoard's), whether it is
//!      DarwinBG, and the submitting thread's QoS (OneiOSAdreno.cpp oneios_gpu_hint, one byte each in the virtio
//!      header's padding of every SUBMIT_3D and in CTX_CREATE's context_init bits 8-23). When the CPU agent publishes
//!      a thread group's role, that function is the one place to read it (docs/agents.md).
//!
//!    Until 2026-11-15 (fix-gpu-widgets) the name list held WidgetRenderer*, *Widget*, *Poster*, iconservices* and
//!    *Thumbnail*, all of which draw what the user sees (home-screen, lock-screen, Today and StandBy widgets, the lock
//!    screen's poster, icons): their queues sat at KGSL priority 12, under every app, and at KGSL's lowest level at a
//!    still screen. On the phone (8644ae9) the fences of every process but the render server then took 181 ms on
//!    average, and 5 of Metal's 100 ms fence-wait slices ran out in 10 s.
//! 2. The GPU's LEVEL ([`Policy`]): KGSL lets a client ask for its highest level (KGSL_CONSTRAINT_PWR_MAX), its lowest
//!    (PWR_MIN), or nothing (the governor decides). The top level is asked only for a frame that would miss its
//!    deadline otherwise: the work interval's deadline when the guest sends one, else the frame cadence (8.3 ms at
//!    120 Hz, at most 16.7 ms), judged by the frames' own GPU time (submission or the previous retirement, whichever is
//!    later, to the KGSL timestamp's retirement), per level. Easy frames, and every frame of a background context,
//!    leave the clock to the governor; background contexts get the lowest level while the screen is still, no other
//!    context has GPU work queued, no other process reads their surfaces, and none of their own fences has waited past
//!    bg_bound_us ([`Policy::lowest_for`]). KGSL's lowest level is the whole GPU's, not the context's.
//! 3. NO STARVATION: no class is put below the default's KGSL ringbuffer ([`kgsl_priority`]). KGSL serves its
//!    ringbuffers by strict priority (a lower one runs only while every higher one is empty), so a lower ringbuffer has
//!    no bound on its fences, and a queue's priority cannot change after it is made while its owner's class can. Within
//!    one ringbuffer KGSL takes its contexts in turn: that is the background contexts' minimum share.

/// A context's class (enum og_class: the values the renderer's patch 0008 takes).
#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Class {
    #[default]
    Default = 0,
    Render = 1,
    Front = 2,
    Background = 3,
}

impl Class {
    /// og_class_name
    pub fn name(self) -> &'static str {
        match self {
            Class::Render => "render server",
            Class::Front => "front",
            Class::Background => "background",
            Class::Default => "default",
        }
    }
}

// The guest's hint (OneiOSAdreno.cpp oneios_gpu_hint): byte 0 the task, byte 1 the thread's QoS, byte 2 the deadline.
/// byte 0: the guest sent it (0: an older guest, no hint)
pub const HINT_VALID: u8 = 0x80;
/// byte 0: the task is DarwinBG
pub const HINT_DARWINBG: u8 = 0x40;
/// byte 0: the task's effective role, task_role_t (0-9)
pub const HINT_ROLE: u8 = 0x0f;
/// byte 2: time to the frame's deadline in 250 us units (0: none; 255: 63.75 ms or more)
pub const HINT_DEADLINE_US: u32 = 250;

// task_role_t (osfmk/mach/task_policy.h)
pub const ROLE_UNSPECIFIED: u8 = 0;
pub const ROLE_FOREGROUND: u8 = 1;
pub const ROLE_BACKGROUND: u8 = 2;
pub const ROLE_CONTROL: u8 = 3;
pub const ROLE_GRAPHICS_SERVER: u8 = 4;
pub const ROLE_THROTTLE: u8 = 5;
pub const ROLE_NONUI: u8 = 6;
pub const ROLE_DEFAULT: u8 = 7;
pub const ROLE_DARWINBG: u8 = 8;
pub const ROLE_USER_INIT: u8 = 9;
// THREAD_QOS_* (osfmk/mach/thread_policy_private.h)
pub const QOS_UNSPECIFIED: u8 = 0;
pub const QOS_MAINTENANCE: u8 = 1;
pub const QOS_BACKGROUND: u8 = 2;
pub const QOS_UTILITY: u8 = 3;
pub const QOS_LEGACY: u8 = 4;
pub const QOS_USER_INITIATED: u8 = 5;
pub const QOS_USER_INTERACTIVE: u8 = 6;

/// Process names (p_name, what proc_selfname gives the context: at most 32 characters). Forms: a name, "X*" (a
/// prefix), "*X*" (a part), separated by commas. The background list names only daemons whose GPU work never reaches
/// the screen:
/// - photoanalysisd: the Photos library's scene, face and memory analysis (Vision and Core ML), hours of it after a restore;
/// - mediaanalysisd: the same for video and for Live Text and Visual Look Up indexing;
/// - assetsd: the Photos library's server: derivatives and thumbnails written to files the Photos app reads later;
/// - searchd: Spotlight's indexer (its results are drawn by SpringBoard's Spotlight, not by it);
/// - analyticsd: diagnostics and usage data;
/// - suggestd: Siri Suggestions' indexing of mail, messages and contacts.
///
/// No pattern: a pattern names processes nobody listed. Even a listed one is not background once the render server
/// reads one of its surfaces ([`class_of`]'s `shown`).
pub const DEFAULT_BACKGROUND: &str = "photoanalysisd,mediaanalysisd,assetsd,searchd,analyticsd,suggestd";
pub const DEFAULT_FRONT: &str = "SpringBoard";

/// Whether `name` (no "xnu:") matches one comma-separated item of `list`.
pub fn name_listed(name: &[u8], list: &str) -> bool {
    list.split(',').any(|item| {
        let item = item.as_bytes();
        let lead = item.first() == Some(&b'*');
        let trail = item.len() > lead as usize && item.last() == Some(&b'*');
        let word = &item[lead as usize..item.len() - trail as usize];
        if word.is_empty() {
            false
        } else if lead {
            name.windows(word.len()).any(|w| w == word) // *X*: a part (or *X: also a part)
        } else if trail {
            name.starts_with(word) // X*
        } else {
            name == word // X
        }
    })
}

/// The class the guest's hint gives (byte 0 the task, byte 1 the thread's QoS).
pub fn class_by_hint(hint: u8, qos: u8) -> Class {
    if hint & HINT_VALID == 0 {
        return Class::Default;
    }
    if hint & HINT_DARWINBG != 0 {
        return Class::Background;
    }
    match hint & HINT_ROLE {
        ROLE_FOREGROUND | ROLE_CONTROL | ROLE_GRAPHICS_SERVER | ROLE_USER_INIT => return Class::Front,
        ROLE_BACKGROUND | ROLE_THROTTLE | ROLE_NONUI | ROLE_DARWINBG => return Class::Background,
        _ => {}
    }
    if (QOS_MAINTENANCE..=QOS_UTILITY).contains(&(qos & 7)) {
        Class::Background
    } else {
        Class::Default
    }
}

/// The class of a context named `name` (as CTX_CREATE gave it: "xnu:<process>") whose submitting task sent `hint`
/// (byte 0) and `qos` (byte 1); `shown`: the render server reads one of its surfaces. `background` and `front` are the
/// name lists (None: the defaults above). A widget renderer is a non-UI process to RunningBoard (and its threads may be
/// at utility QoS) while its surfaces are on the home screen: `shown` keeps it out of the background.
pub fn class_of(name: &[u8], hint: u8, qos: u8, shown: bool, background: Option<&str>, front: Option<&str>) -> Class {
    let name = name.strip_prefix(b"xnu:").unwrap_or(name);
    if name == b"backboardd" {
        return Class::Render;
    }
    if name_listed(name, front.unwrap_or(DEFAULT_FRONT)) {
        return Class::Front;
    }
    let c = if name_listed(name, background.unwrap_or(DEFAULT_BACKGROUND)) {
        Class::Background
    } else {
        class_by_hint(hint, qos)
    };
    if c == Class::Background && shown {
        Class::Default
    } else {
        c
    }
}

/// The KGSL priority (0 highest, 15 lowest; four to a ringbuffer, KGSL_CONTEXT_PRIORITY_SHIFT 12) of a class's queues:
/// the render server and the front app 4 (the second ringbuffer), everything else 8 (the third), background included.
/// Not 12 (the fourth, 2026-11-15 #42 to fix-gpu-widgets): KGSL runs a lower ringbuffer only while every higher one is
/// empty, so its fences had no bound, and the class a queue was made with (by name or by role at the context's
/// creation) stays its priority for good. None takes the first ringbuffer (0-3, Android's compositor's). Every class
/// gets an explicit one, so the order holds whatever the kernel's default for KGSL_CONTEXT_PRIORITY_UNDEF is.
/// The KGSL priority (0 highest, 15 lowest; four to a ringbuffer, KGSL_CONTEXT_PRIORITY_SHIFT 12) of a class's queues.
/// With `ui_rb = false` (`gpu-kgsl-ui-rb=0`, default), every class gets 8 (third ringbuffer), keeping iOS's render server
/// and front app out of Android's UI ringbuffer (4-7). With `ui_rb = true` (`gpu-kgsl-ui-rb=1`), render server and front
/// app get 4 (second ringbuffer), everything else 8.
pub fn kgsl_priority_for(c: Class, ui_rb: bool) -> u32 {
    match c {
        Class::Render | Class::Front if ui_rb => 4,
        _ => 8,
    }
}

pub fn kgsl_priority(c: Class) -> u32 {
    kgsl_priority_for(c, true)
}

// ---- whose surfaces another process reads ------------------------------------------------------------------------------
// A blob over guest pages (an IOSurface the process draws into, or one it imports) is known by its first guest page:
// two contexts with blobs over the same first page share that surface (the guest's oneios-gpu-pixels "pages of another
// process's surfaces" matches the same way). Pages::note is called for each such blob as it is made; it returns the
// entry another context left for the same page. The table is a cache: an entry is replaced by the next blob over its
// page, or, when the probe run is full, the oldest entry of the run goes; nothing is removed at unref, so the caller
// checks that the other resource still lives.
pub const PAGES: usize = 8192;
pub const PAGES_PROBE: usize = 8;

/// One entry of [`Pages`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub page: u64,
    pub seq: u64,
    pub res: u32,
    pub ctx: u32,
}

/// The table of first pages.
pub struct Pages {
    slot: Box<[Page; PAGES]>,
    seq: u64,
}

impl Default for Pages {
    fn default() -> Pages {
        Pages { slot: Box::new([Page::default(); PAGES]), seq: 0 }
    }
}

impl Pages {
    /// Context `ctx` made resource `res` over guest pages from `page`: the entry another context left for that page
    /// (None: none).
    pub fn note(&mut self, page: u64, res: u32, ctx: u32) -> Option<Page> {
        if page == 0 || res == 0 {
            return None;
        }
        let key = page >> 12;
        let start = (key.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 51) as usize & (PAGES - 1);
        let mut pick = start;
        let mut other = None;
        for i in 0..PAGES_PROBE {
            let at = (start + i) & (PAGES - 1);
            let s = self.slot[at];
            if s.page == page {
                pick = at;
                if s.ctx != ctx {
                    other = Some(s);
                }
                break;
            }
            if s.page == 0 {
                pick = at;
                break;
            }
            if s.seq < self.slot[pick].seq {
                pick = at;
            }
        }
        self.seq += 1;
        self.slot[pick] = Page { page, seq: self.seq, res, ctx };
        other
    }
}

// ---- the level --------------------------------------------------------------------------------------------------------
/// The level now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    #[default]
    Governor = 0,
    Top = 1,
}

/// gpu-level=deadline|hold|governor
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Deadline = 0,
    Hold = 1,
    Governor = 2,
}

/// What the caller does for a frame: nothing new, hold the top level (until hold_ms after this frame), or let it go now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Top,
    Governor,
}

/// What a frame was given, for [`Policy::frame_done`] (budget_us 0: the frame does not count).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub level: Level,
    pub cold: bool,
    pub budget_us: u32,
}

/// The level's settings, state and counters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    // settings (Policy::new's defaults)
    pub mode: Mode,
    /// the budget of a frame without a deadline, at 120 Hz: 8333
    pub budget_us: u32,
    /// the longest cadence budget (60 Hz): 16667
    pub slow_budget_us: u32,
    /// the top clock over the lowest, x16 (KGSL's gpu_available_frequencies; 64 = 4.0)
    pub ratio_x16: u32,
    /// a frame fits when it takes at most this share of its budget: 80
    pub fit_pct: u32,
    /// easy frames in a row at the top before the governor gets the clock back: 8
    pub down_frames: u32,
    /// a pause after which the next frame starts a burst (the governor is at its bottom): 100 ms
    pub idle_us: u32,
    /// another context's frames count within this of a render-server frame: 1 s
    pub follow_us: u32,
    /// the screen still this long: background contexts at the lowest level: 500 ms
    pub quiet_us: u32,
    /// a background fence that waited this long ends the lowest level (lowest_for): 50 ms
    pub bg_bound_us: u32,
    // state
    pub level: Level,
    /// the level the caller was last told
    pub told: Level,
    pub last_render_ns: u64,
    pub last_frame_ns: u64,
    pub last_bg_min_ns: u64,
    /// the render server's frame interval in a burst (average)
    pub interval_us: u32,
    /// GPU time of a frame (average) at the top, at the governor's, at a burst's start
    pub top_us: u32,
    pub gov_us: u32,
    pub cold_us: u32,
    /// a frame's GPU time at the governor's clock during a burst over its time at the top, x16, as measured (0: not
    /// yet; then [`DOWN_GUESS_X16`]). The governor is not at its bottom in a burst: KGSL's ratio (ratio_x16, 12.5 on
    /// the S24 Ultra) said no frame over 0.53 ms at the top would ever fit at 120 Hz, so the level never came down
    pub gov_x16: u32,
    /// gpu-down=bottom (docs/switches.md): the way down judges a frame at KGSL's lowest clock, as before 3b36ff0
    pub down_at_bottom: bool,
    pub easy: u32,
    pub down_need: u32,
    // counters (since the start)
    pub frames: u64,
    pub frames_top: u64,
    pub ups: u64,
    pub downs: u64,
    pub misses: u64,
    pub late: u64,
    /// background submissions kept off the lowest level by lowest_for's rules
    pub lowest_refused: u64,
    /// other work that ended a background lowest level with the top (frame_start)
    pub lowest_undone: u64,
}

/// The guest's Metal runtime waits for a fence in slices of 100 ms (metal2vulkan command_buffer/completion.rs
/// WAIT_SLICE) and waits again after each; the tunnel counts each slice that runs out as one of oneios-gpu-report's
/// "fence timeouts". A fence that took this long from submission to retirement on the host is one of those (xnuhost's
/// 10 s "fences over 100 ms" line).
pub const FENCE_SLOW_US: u64 = 100_000;

/// The governor's clock in a burst before one is measured: half the top's (x16). The first frames at the governor teach
/// the real ratio ([`Policy::gov_x16`]); a frame that (nearly) misses there sends the level back up at once.
pub const DOWN_GUESS_X16: u32 = 32;

fn avg(avg: u32, sample: u32) -> u32 {
    if avg != 0 {
        ((avg as u64 * 3 + sample as u64 + 2) / 4) as u32
    } else if sample != 0 {
        sample
    } else {
        1
    }
}

impl Policy {
    /// og_policy_init: the settings' defaults, nothing seen yet.
    pub fn new(mode: Mode) -> Policy {
        Policy {
            mode,
            budget_us: 8333,
            slow_budget_us: 16667,
            ratio_x16: 64,
            fit_pct: 80,
            down_frames: 8,
            down_need: 8,
            idle_us: 100_000,
            follow_us: 1_000_000,
            quiet_us: 500_000,
            bg_bound_us: 50_000,
            ..Policy::default()
        }
    }

    fn fit(&self, budget_us: u32) -> u32 {
        (budget_us as u64 * self.fit_pct as u64 / 100) as u32
    }

    /// A frame of `top_us` at the top, at the governor's clock in a burst: the measured ratio ([`Policy::gov_x16`]),
    /// else [`DOWN_GUESS_X16`], never more than KGSL's own top over lowest.
    pub fn at_governor(&self, top_us: u32) -> u32 {
        let r = if self.gov_x16 != 0 { self.gov_x16 } else { DOWN_GUESS_X16.min(self.ratio_x16.max(16)) };
        (top_us as u64 * r as u64 / 16) as u32
    }

    /// A frame of `top_us` at the top, at the lowest clock (ratio_x16).
    pub fn at_bottom(&self, top_us: u32) -> u32 {
        (top_us as u64 * self.ratio_x16 as u64 / 16) as u32
    }

    /// A fenced submission of a context of class `cls` is about to reach KGSL at `now_ns`; `deadline_us` is the time
    /// to its frame's deadline from the guest (0: none). Returns what to do with the level, and what the frame was
    /// given for [`frame_done`](Self::frame_done) (budget_us 0: the frame does not count).
    pub fn frame_start(&mut self, cls: Class, now_ns: u64, deadline_us: u32) -> (Action, Frame) {
        let none = (Action::None, Frame::default());
        if cls == Class::Background || self.mode == Mode::Governor {
            return none;
        }
        if cls != Class::Render
            && (self.last_render_ns == 0 || now_ns - self.last_render_ns > self.follow_us as u64 * 1000)
        {
            // Not a counted frame. But a background context's lowest level may still be in force (KGSL keeps it until
            // it expires, and only a faster level ends it sooner): this work, which another process may wait for (a
            // widget's drawing at a still screen), would run at the lowest clock. The top ends it, once per lowest
            // episode.
            if self.mode == Mode::Deadline && self.last_bg_min_ns != 0 && now_ns - self.last_bg_min_ns < 200_000_000 {
                self.last_bg_min_ns = 0;
                self.told = Level::Top;
                self.lowest_undone += 1;
                return (Action::Top, Frame::default());
            }
            return none;
        }
        let gap_us = if self.last_frame_ns != 0 { (now_ns - self.last_frame_ns) / 1000 } else { u64::MAX };
        let cold = gap_us >= self.idle_us as u64;
        if cls == Class::Render {
            if !cold && self.last_render_ns != 0 {
                let since = (now_ns - self.last_render_ns) / 1000;
                if since >= 500 {
                    // a second submission of one frame is not an interval
                    self.interval_us = avg(self.interval_us, since.min(u32::MAX as u64) as u32);
                }
            }
            self.last_render_ns = now_ns;
        }
        self.last_frame_ns = now_ns;
        let mut budget = self.budget_us;
        if deadline_us != 0 {
            budget = deadline_us.max(1000);
        } else if !cold && self.interval_us > budget {
            budget = self.interval_us.min(self.slow_budget_us);
        }
        let fit = self.fit(budget);
        let mut want = self.level;
        if self.mode == Mode::Hold {
            want = Level::Top; // 0005's rule: every frame at the top
        } else if cold {
            // The governor sits at its lowest level after a pause: a frame known to take that long there gets the top.
            let predict = if self.cold_us != 0 {
                self.cold_us
            } else if self.top_us != 0 {
                self.at_bottom(self.top_us)
            } else {
                0
            };
            want = if predict == 0 || predict > fit { Level::Top } else { Level::Governor };
            // Background contexts were at the lowest level just now: KGSL keeps that until it expires unless a faster
            // one comes.
            if self.last_bg_min_ns != 0 && now_ns - self.last_bg_min_ns < 200_000_000 {
                want = Level::Top;
            }
            self.easy = 0;
            self.down_need = self.down_frames;
        } else if self.level == Level::Governor && self.gov_us > fit {
            want = Level::Top;
        }
        if want != self.level {
            if want == Level::Top {
                self.ups += 1;
            } else {
                self.downs += 1;
            }
            self.easy = 0;
        }
        self.level = want;
        self.frames += 1;
        if want == Level::Top {
            self.frames_top += 1;
        }
        let frame = Frame { level: want, cold, budget_us: budget };
        let action = if want == Level::Top {
            Action::Top
        } else if self.told == Level::Top {
            Action::Governor
        } else {
            Action::None
        };
        self.told = want;
        (action, frame)
    }

    /// The frame `f` is done: `gpu_us` from its submission (or the previous retirement of its ring, if later) to its
    /// KGSL timestamp's retirement.
    pub fn frame_done(&mut self, f: &Frame, gpu_us: u32) {
        if f.budget_us == 0 {
            return;
        }
        let fit = self.fit(f.budget_us);
        if gpu_us > f.budget_us {
            self.late += 1;
        }
        if f.level == Level::Top {
            self.top_us = avg(self.top_us, gpu_us);
            if self.mode != Mode::Deadline {
                return;
            }
            // Whether it would fit at the governor's clock (a burst's: not its bottom, which a cold frame meets).
            let cost = if self.down_at_bottom { self.at_bottom(gpu_us) } else { self.at_governor(gpu_us) };
            self.easy = if cost <= fit { self.easy + 1 } else { 0 };
            if self.level == Level::Top && self.easy >= self.down_need {
                self.level = Level::Governor;
                self.downs += 1;
                self.easy = 0;
            }
            return;
        }
        self.gov_us = avg(self.gov_us, gpu_us);
        if f.cold {
            self.cold_us = avg(self.cold_us, gpu_us);
        } else if self.top_us != 0 {
            // What the governor's clock costs this work in a burst, against the top's (bounded by KGSL's own ratio).
            let r = (gpu_us as u64 * 16 / self.top_us as u64).clamp(16, self.ratio_x16.max(16) as u64) as u32;
            self.gov_x16 = if self.gov_x16 != 0 { ((self.gov_x16 as u64 * 3 + r as u64 + 2) / 4) as u32 } else { r };
        }
        if gpu_us > fit {
            // It (nearly) missed at the governor's clock: the next frames at the top, and longer before trying again.
            self.misses += 1;
            if self.level != Level::Top {
                self.level = Level::Top;
                self.ups += 1;
            }
            self.easy = 0;
            self.down_need = if self.down_need < 64 { self.down_need * 4 } else { 64 };
        }
    }

    /// Whether background contexts get the lowest level now: no counted frame for quiet_us (the screen is still).
    pub fn background_min(&mut self, now_ns: u64) -> bool {
        if self.mode != Mode::Deadline
            || (self.last_frame_ns != 0 && now_ns - self.last_frame_ns < self.quiet_us as u64 * 1000)
        {
            return false;
        }
        self.last_bg_min_ns = now_ns;
        true
    }

    /// Whether a background context's submission at `now_ns` carries KGSL's lowest level. KGSL's lowest is the whole
    /// GPU's (it holds until it expires unless a faster level comes), so it never applies while:
    /// - another context has GPU work queued (`others_pending`: fences of non-background contexts not yet retired):
    ///   that work would run at the lowest clock too (a widget renderer drawing at a still screen counts no frame for
    ///   the level);
    /// - another process reads this context's surfaces (`shared`): someone waits for this work;
    /// - this context's oldest fence not yet retired was submitted bg_bound_us ago or more (`oldest_ns`, 0: none):
    ///   aging, the work that waited past the bound runs at the governor's clock, so it is done well inside Metal's
    ///   100 ms wait slice.
    ///
    /// Otherwise the screen's stillness decides ([`background_min`](Self::background_min)).
    pub fn lowest_for(&mut self, now_ns: u64, others_pending: u32, shared: bool, oldest_ns: u64) -> bool {
        if self.mode != Mode::Deadline {
            return false;
        }
        if others_pending != 0
            || shared
            || (oldest_ns != 0 && now_ns > oldest_ns && now_ns - oldest_ns >= self.bg_bound_us as u64 * 1000)
        {
            if self.last_frame_ns == 0 || now_ns - self.last_frame_ns >= self.quiet_us as u64 * 1000 {
                self.lowest_refused += 1;
            }
            return false;
        }
        self.background_min(now_ns)
    }
}

#[cfg(test)]
mod tests;
