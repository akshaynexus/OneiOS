// SPDX-License-Identifier: MIT
//! oneios-audio: the AAudio end of the audio bridge (C: audio/aaudio_helper.c; audio/backend.rs explains why it is a
//! process). Run by xnuhost as `oneios-audio FD`: FD is a SOCK_SEQPACKET socket carrying audio/proto.rs. AAudio itself
//! is behind the [`AAudio`] and [`AStream`] traits: the NDK's in the bin (audio/oneios_audio.rs), a fake in the test.
//!
//! Output: 48 kHz S16 stereo periods from the guest go into a ring. A writer thread of this process (normal priority)
//! moves them into AAudio in chunks of 240 frames with a blocking AAudioStream_write. What decides the latency is the
//! level: the frames in the ring plus those AAudio holds and its server has not read yet.
//!   - The server's pulls: a normal mixer track is read by AudioFlinger's mixer a whole mixer period at a time
//!     (AAudioStream_getFramesRead jumps by it: 1600 frames, 33 ms, on the phone's primary output [I]), and the mixer
//!     takes a track's frames only when it holds a whole period. The writer watches the jumps: their size (the median
//!     of the last 16) and the time of the last one. The level the stream needs is then the frames the next pull takes
//!     that have come in since the last one, plus a margin (the target): right after a pull only the target. Until the
//!     pulls are seen (a new stream) it is the old, phase-blind rule: one AAudio burst plus the target at every moment,
//!     which averages half a period more (and up to a whole one) than needed.
//!   - Before the guest's first chunk, and after AAudio ran dry (an underrun: the missing part is silence), silence
//!     fills the level to that need (priming). Each underrun raises the target by 5 ms, to at most 80 ms; each 20 s
//!     (ONEIOS_AUDIO_TARGET_DECAY_S) without one lowers it by 5 ms, back to ONEIOS_AUDIO_TARGET_MS (20).
//!   - Clock drift and excess: while the level (averaged over about 1 s) is more than 5 ms above the need, each chunk of
//!     240 ring frames is played as 239 frames (237 above 10 ms: 1.25 %), and as 241 while it is 5 ms below (linear
//!     interpolation, never by cutting sound). A new stream starts 20-40 ms above (AudioFlinger's start threshold and
//!     the phase of its first pull), which goes in 2-3 s. Until the pulls are seen, the old reference stands: the level
//!     1 s after the start, and a ring above the target plus 10 ms is played faster.
//!   - AAudio's buffer size (AudioFlinger's start threshold, and the most a blocking write leaves queued) is a burst
//!     plus the target plus one chunk (ONEIOS_AUDIO_BURSTS=N: N bursts instead), set again when the target moves.
//!   - Silence is cut, sound is not: while the level is more than 10 ms above the need, a chunk at the ring's head that
//!     is digital silence (every sample within +-1) is dropped. A backlog (a new track's stall of about 200 ms, a burst
//!     of late periods) thus goes at the first pause of the sound instead of over 16 s of faster playing.
//!   - A ring that holds more than the target plus 60 ms drops its oldest audio down to the target (an overflow). Until
//!     the server has taken the stream's first frames the limit is the whole ring (500 ms), then what the stall left
//!     plus 40 ms, shrinking as it plays: on the phone a cut there lost the first 120 ms of iOS's first sound;
//!   - the guest's STOP keeps the stream open for ONEIOS_AUDIO_LINGER_S (60 s): what the ring still holds plays, then
//!     silence; iOS starts and stops its device around each sound, and a reopened track stalls again (the open and the
//!     new track's first ~200 ms before the server takes frames). Then it is closed. 60 s covers a typing or a browsing
//!     session's pauses; the price is the phone's speaker path kept on with silence for that minute.
//!
//! Input: a reader thread reads the microphone in periods of 240 frames (blocking AAudioStream_read) and sends each one
//! to xnuhost.
//!
//! Volume (2026-11-22): iOS's volume is applied in the guest (the Speaker's hardware volume control); the helper sets
//! One UI's media index on the speaker to its maximum for its run and back at its end (host_volume_full below).
//!
//!
//! The way back to One UI (docs/hardware.md section 6): no thread of this process may run realtime. AAudio makes a
//! realtime (SCHED_FIFO) thread only for a data callback (AudioStream::createThread_l -> registerThread, which asks
//! system_server's scheduling_policy service; in a session that service is gone). So: no data callback, ever: one
//! blocking writer thread at SCHED_OTHER, with a uclamp.min boost (ONEIOS_AUDIO_UCLAMP, 512 of 1024: the governor's
//! clock and the placement, never a policy change).
//!
//! The speaker's path (since 2026-11-15, fix-audio-latency; AOSP main read, not One UI's own build):
//!   1. MMAP exclusive (the default, ONEIOS_AUDIO_MMAP=exclusive): LOW_LATENCY, sharing EXCLUSIVE, MMAP allowed
//!      (AAudio_setMMapPolicy AUTO). The DSP reads our buffer itself (the `mmap_no_irq_out` port): no mixer and no
//!      thread in between, and nothing asks for a priority. Our writer keeps the buffer ahead of the DSP's read; the
//!      level is the target above the frames the DSP takes before the guest's next period.
//!   2. MMAP shared only while system_server runs (ONEIOS_AUDIO_MMAP=shared asks it first): its mixer is audioserver's
//!      AAudio endpoint thread, which registers itself (-> android::requestPriority), and without the scheduling_policy
//!      service that call waits for it forever. A stream opened shared in a session is closed before its start and
//!      opened again on 3.
//!   3. Legacy (ONEIOS_AUDIO_MMAP=off, or 1 and 2 not given): MMAP refused (AAudio_setMMapPolicy NEVER), LOW_LATENCY on
//!      a normal mixer track (libaudioclient takes FAST off a synchronous native writer; what LOW_LATENCY still sends
//!      is AUDIO_OUTPUT_FLAG_RAW). ONEIOS_AUDIO_PERF=none: performance mode NONE (the path before 2026-11-15).
//!
//! A stream whose server takes no frames: on the S25's HAL an MMAP exclusive stream sometimes never leaves STARTING (no
//! timestamp, every blocking write times out with 0, the DSP never reads: sessions 3, 4 and 5), and the writer would
//! drop every guest period for as long as the track lingered. Two places notice and reopen the speaker on 3, once for
//! the helper's run (mmap_dead below): the probe, when the writer says the stream is dead or after 1.5 s with no read
//! (before the guest's boot, so iOS's first sound already finds the mixer track; the boot costs about a second more),
//! and the writer, when three writes in a
//! row take nothing (600 ms: the stream never started, so AAudio holds nothing) or AAudio holds frames the server has
//! not read for 1 s (it stopped; the receive loop closes the stream and opens the mixer track while the guest's stream
//! stays up: a gap of the ring's fill). The fallback is logged; after it no open asks for MMAP again.
//!
//! And a guard: every thread of the process is checked after each start and every 10 s, and one found SCHED_FIFO or
//! SCHED_RR is set back to SCHED_OTHER (logged).
//!
//! End to end (the `end-to-end` of the speaker's report): iOS's sample time to the speaker. The guest's engine hands
//! each period on at a fixed time of its clock, on the same counter as the host's CLOCK_MONOTONIC_RAW; the earliest
//! arrival of the last 1-2 s fixes that schedule, each period's lateness is its arrival against it, and the host's part
//! is what the ring and AAudio hold ahead of it when it arrives (AAudioStream_getTimestamp: written but not yet
//! presented).
//!
//! The probe (AU_PROBE, xnuhost before the boot): the speaker stream opened in silence, its latency measured and sent
//! back, so the kernel publishes the host's measured latency as the device's output latency. A stream that takes no
//! frame (the writer's 600 ms, or the probe's 1.5 s) is reopened on 3 (above); a mixer track's silence is not measured
//! (0: the guest's first period starts it).
//!
//!
//! Log lines: "oneios-audio: ..." on stderr (xnuhost's log): each stream's open (its mode), the server's pulls once they
//! are seen, target changes, a report every 10 s while a stream runs, and one at its stop: frames, underruns,
//! overflows, slips, silence cut, the ring's fill, the level against its need, the latency.

use super::proto::{
    AuHeader, AU_HEADER, AU_LATENCY, AU_MAX_FRAMES, AU_PCM, AU_PROBE, AU_START, AU_STATE, AU_STOP, AU_TOLD,
};
use crate::proto::Wire;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;
use xh_core::sys::{errno, strerror};
use xh_core::time;

// ---- AAudio, as the helper sees it ----------------------------------------------------------------
pub const AAUDIO_OK: i32 = 0;
pub const PERF_NONE: i32 = 10;
pub const PERF_POWER_SAVING: i32 = 11;
pub const PERF_LOW_LATENCY: i32 = 12;
pub const SHARING_EXCLUSIVE: i32 = 0;
pub const SHARING_SHARED: i32 = 1;
/// AAudio_setMMapPolicy's values (AAudioTesting.h).
pub const POLICY_NEVER: i32 = 1;
pub const POLICY_AUTO: i32 = 2;

/// What a stream is opened with (the builder's settings; the format is always PCM_I16, the speaker usage MEDIA and
/// content MUSIC, the microphone preset VOICE_RECOGNITION).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamRequest {
    pub output: bool,
    pub rate: u32,
    pub channels: u32,
    pub perf: i32,
    pub sharing: i32,
}

/// Which of libaudioclient's AudioSystem volume calls were found (by their mangled names).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VolumeCalls {
    pub get: bool,
    pub max: bool,
    pub set: bool,
    pub set_muted: bool,
}

/// AAudio and the process-wide calls next to it. Results are AAudio's (0 OK, negative errors).
pub trait AAudio: Send + Sync {
    fn open(&self, r: &StreamRequest) -> Result<Arc<dyn AStream>, i32>;
    fn result_text(&self, rc: i32) -> String;
    /// AAudio_setMMapPolicy (process-wide, read at each open); None when libaaudio does not export it.
    fn set_mmap_policy(&self, policy: i32) -> Option<i32>;
    fn volume_calls(&self) -> VolumeCalls;
    /// getVolumeIndexForAttributes(media, speaker): (status, index).
    fn volume_index(&self) -> (i32, i32);
    /// getMaxVolumeIndexForAttributes(media): (status, index).
    fn volume_max(&self) -> (i32, i32);
    /// setVolumeIndexForAttributes(media, index, [muted false,] speaker): its status. The `muted` form when there is one.
    fn set_volume_index(&self, index: i32) -> i32;
}

/// One AAudio stream; every call may come from any thread (AAudio's own rules).
pub trait AStream: Send + Sync {
    fn request_start(&self) -> i32;
    fn request_stop(&self) -> i32;
    fn close(&self);
    /// A blocking write of `frames` interleaved frames from `pcm`: frames written, or a negative error.
    fn write(&self, pcm: &[i16], frames: i32, timeout_ns: i64) -> i32;
    fn read(&self, pcm: &mut [i16], frames: i32, timeout_ns: i64) -> i32;
    fn set_buffer_size(&self, frames: i32) -> i32;
    /// AAudioStream_getTimestamp(CLOCK_MONOTONIC): (position, time_ns), None when it has none.
    fn timestamp(&self) -> Option<(i64, i64)>;
    fn frames_per_burst(&self) -> i32;
    fn buffer_size(&self) -> i32;
    fn buffer_capacity(&self) -> i32;
    fn sample_rate(&self) -> i32;
    fn channel_count(&self) -> i32;
    fn device_id(&self) -> i32;
    fn session_id(&self) -> i32;
    fn xrun_count(&self) -> i32;
    fn sharing_mode(&self) -> i32;
    fn performance_mode(&self) -> i32;
    fn frames_written(&self) -> i64;
    fn frames_read(&self) -> i64;
    /// AAudioStream_isMMapUsed: 1, 0, or -1 when libaaudio does not export it.
    fn mmap_used(&self) -> i32;
}

// ---- the numbers ----------------------------------------------------------------------------------
pub const RATE: u32 = 48000;
/// 500 ms of stereo at 48 kHz (a new track's stall and a burst)
pub const RING_FRAMES: u32 = 24000;
/// the largest AAudio burst taken as it is
const MAX_BURST: u32 = 9600;
const IN_PERIOD: usize = 240;
/// the writer's unit (one guest period)
pub const CHUNK: u32 = 240;

pub const fn frames_ms(f: u32) -> u32 {
    (f as u64 * 1000 / RATE as u64) as u32
}
pub const fn ms_frames(ms: u32) -> u32 {
    ms * (RATE / 1000)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MmapWant {
    Off,
    Shared,
    Exclusive,
}

/// The speaker stream's path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutPath {
    #[default]
    Legacy,
    MmapShared,
    MmapExclusive,
}

fn path_name(path: OutPath) -> &'static str {
    match path {
        OutPath::MmapExclusive => "MMAP exclusive: the DSP reads the buffer",
        OutPath::MmapShared => "MMAP shared: audioserver's AAudio endpoint mixes it",
        OutPath::Legacy => "a mixer track",
    }
}

fn perf_name(mode: i32) -> &'static str {
    match mode {
        PERF_LOW_LATENCY => "low latency",
        PERF_POWER_SAVING => "power saving",
        _ => "none",
    }
}

/// CLOCK_MONOTONIC in nanoseconds, from its seconds (as the C's double).
fn now_ns() -> i64 {
    (time::monotonic_s() * 1e9) as i64
}

/// The helper's settings, from the environment at its start.
#[derive(Clone, Debug)]
pub struct Settings {
    /// ONEIOS_AUDIO_TARGET_MS (0: the path's default: 5 ms MMAP, 20 ms a mixer)
    pub target_env_ms: u32,
    pub target_decay_s: u32,
    /// ONEIOS_AUDIO_BURSTS; 0: the buffer follows the need (buffer_fit)
    pub buffer_bursts: u32,
    pub linger_s: u32,
    /// ONEIOS_AUDIO_UCLAMP (0..1024; 0: none)
    pub uclamp_min: u32,
    /// ONEIOS_AUDIO_PERF=none: NONE
    pub perf_mode: i32,
    /// ONEIOS_AUDIO_MMAP=exclusive|shared|off
    pub mmap_want: MmapWant,
    /// ONEIOS_AUDIO_PROBE_PREFILL=1: the AU_PROBE speaker stream is filled with silence (non-blocking writes, its
    /// whole buffer) before requestStart. Off by default (the S24 Ultra's probe is unchanged); the S26 Ultra's row of
    /// the host table sets it: there a stream started underfilled took 480 of 576 frames and never moved FramesRead
    /// or gave a timestamp (YMGPwcca/OneiOS 7cf70ce7, 4dd7242c). A guest START's stream is never prefilled.
    pub probe_prefill: bool,
}

fn env_ms(name: &str, fallback: u32, low: u32, high: u32) -> u32 {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => {
            let n = xh_core::parse::leading_u64(&v, 10).map_or(0, |(n, _)| n.min(u64::from(u32::MAX)) as u32);
            n.clamp(low, high)
        }
        _ => fallback,
    }
}

impl Settings {
    pub fn from_env() -> Settings {
        let perf = std::env::var("ONEIOS_AUDIO_PERF").unwrap_or_default();
        let mmap = std::env::var("ONEIOS_AUDIO_MMAP").unwrap_or_default();
        Settings {
            target_env_ms: env_ms("ONEIOS_AUDIO_TARGET_MS", 0, 2, 150),
            target_decay_s: env_ms("ONEIOS_AUDIO_TARGET_DECAY_S", 20, 1, 3600),
            buffer_bursts: env_ms("ONEIOS_AUDIO_BURSTS", 0, 1, 8),
            linger_s: env_ms("ONEIOS_AUDIO_LINGER_S", 60, 0, 600),
            uclamp_min: env_ms("ONEIOS_AUDIO_UCLAMP", 512, 0, 1024),
            perf_mode: if perf == "none" { PERF_NONE } else { PERF_LOW_LATENCY },
            mmap_want: match mmap.as_str() {
                "" | "exclusive" => MmapWant::Exclusive,
                "shared" => MmapWant::Shared,
                _ => MmapWant::Off,
            },
            probe_prefill: env_ms("ONEIOS_AUDIO_PROBE_PREFILL", 0, 0, 1) == 1,
        }
    }
}

/// The speaker's counters since its START (and the end to end since the last report).
#[derive(Clone, Copy, Debug, Default)]
pub struct OutStats {
    pub r#in: u64,
    pub played: u64,
    pub underruns: u64,
    pub overflows: u64,
    pub dropped: u64,
    pub slips_up: u64,
    pub slips_down: u64,
    pub silence: u64,
    pub fill_min: u32,
    pub fill_max: u32,
    /// the level less what the next pull takes (frames; averaged once the pulls are known)
    pub margin: i32,
    /// the highest target since the start (frames; 0: never raised)
    pub target_peak: u32,
    /// AAudio's own underruns (AAudioStream_getXRunCount) seen by the writer
    pub xruns: u64,
    /// End to end, per guest period that arrived while AAudio had a timestamp: iOS's sample time to the speaker, and
    /// the guest's lateness in it (s).
    pub e2e_sum: f64,
    pub e2e_max: f64,
    pub late_sum: f64,
    pub e2e_n: u32,
}

/// The guest's schedule: the frames since its START, and the earliest arrival less their time (CLOCK_MONOTONIC_RAW, s)
/// over the last one to two seconds: this second's (`now_min`, from `since`) and the last's (`last_min`; a large
/// value: none). A rolling minimum, not the stream's: a guest that paused or lost a period would otherwise look late
/// for the rest of the stream.
#[derive(Clone, Copy, Debug)]
struct GuestClock {
    frames: u64,
    now_min: f64,
    last_min: f64,
    since: f64,
}

impl Default for GuestClock {
    fn default() -> GuestClock {
        GuestClock { frames: 0, now_min: 1e30, last_min: 1e30, since: -1.0 }
    }
}

/// The server's pulls, as AAudioStream_getFramesRead shows them (the writer's own; read by the report).
#[derive(Clone, Copy, Debug, Default)]
pub struct Pulls {
    /// frames read when last looked
    read: i64,
    /// the current pull's first and last step
    group_at: f64,
    step_at: f64,
    /// frames in the current pull so far
    group: u32,
    /// the last complete pulls
    sizes: [u32; 16],
    count: u32,
    /// their median (0: fewer than 4 seen)
    pub size: u32,
    /// the time between pulls, averaged (s)
    pub period: f64,
    /// logged once per stream
    told: bool,
}

impl Pulls {
    fn reset(read: i64) -> Pulls {
        Pulls { read, ..Pulls::default() }
    }

    /// A step of frames read: steps less than 3 ms apart are one pull (a mixer may release a period in pieces).
    fn saw(&mut self, read: i64, now: f64) {
        if read < self.read {
            *self = Pulls::reset(read);
            return;
        }
        if read == self.read {
            return;
        }
        let step = (read - self.read) as u32;
        self.read = read;
        if self.group != 0 && now - self.step_at < 0.003 {
            self.group += step;
            self.step_at = now;
            return;
        }
        if self.group != 0 {
            let gap = now - self.group_at;
            self.period = if self.count != 0 { self.period + (gap - self.period) / 8.0 } else { gap };
            self.sizes[self.count as usize % 16] = self.group;
            self.count += 1;
            let n = self.count.min(16) as usize;
            let mut sorted = self.sizes;
            sorted[..n].sort_unstable();
            self.size = if self.count >= 4 { sorted[n / 2] } else { 0 };
        }
        self.group = step;
        self.group_at = now;
        self.step_at = now;
    }

    /// The frames the next pull takes that have come in since the last one (the need above the target), or None when
    /// the pulls are not known (not yet seen, or stopped for longer than three of them).
    fn ahead(&self, now: f64) -> Option<i64> {
        if self.size == 0 {
            return None;
        }
        let elapsed = now - self.group_at;
        if elapsed > 3.0 * f64::from(self.size) / f64::from(RATE) + 0.01 {
            return None;
        }
        let frames = elapsed * f64::from(RATE);
        Some(if frames < f64::from(self.size) { frames as i64 } else { i64::from(self.size) })
    }
}

/// The speaker's ring and everything the writer and the receive loop share (C: what ring_lock guards).
pub struct Ring {
    ring: Vec<i16>,
    pub head: u32,
    pub count: u32,
    /// frames
    pub target: u32,
    priming: bool,
    /// between the guest's START and STOP
    guest_on: bool,
    /// the server has taken frames from this stream (after its start stall)
    consuming: bool,
    /// the most the ring may hold now: the stall's backlog, shrinking as it plays
    allowance: u32,
    /// counts the guest's STARTs: the writer measures its level anew
    generation: u32,
    pub stats: OutStats,
    clock: GuestClock,
    /// The latest guest period (its lateness, s) and the periods since the target last moved or held.
    window_late_max: f64,
    window_periods: u32,
    pub pulls: Pulls,
    /// the level's floor and ceiling of this stream (ms)
    pub target_ms: u32,
    pub target_max_ms: u32,
    pub out_path: OutPath,
    /// its reader (the DSP, AAudio's endpoint) takes frames all the time
    pub continuous: bool,
    /// The writer saw the server take nothing (600 ms of timed-out writes, or a second with AAudio's frames unread) on
    /// a stream that is not the legacy track: the receive loop reopens the speaker on the legacy path (once, mmap_dead
    /// on the Helper).
    dead: bool,
    /// When the target was last raised (CLOCK_MONOTONIC s, -1: not since the stream opened): raise_target's coalescing.
    raised_at: f64,
}

impl Ring {
    fn new() -> Ring {
        Ring {
            ring: vec![0; RING_FRAMES as usize * 2],
            head: 0,
            count: 0,
            target: 0,
            priming: true,
            guest_on: false,
            consuming: false,
            allowance: RING_FRAMES,
            generation: 0,
            stats: OutStats::default(),
            clock: GuestClock::default(),
            window_late_max: 0.0,
            window_periods: 0,
            pulls: Pulls::default(),
            target_ms: 20,
            target_max_ms: 80,
            out_path: OutPath::Legacy,
            continuous: false,
            dead: false,
            raised_at: -1.0,
        }
    }

    /// the ring's level and the writer's target, for the reports and the tests
    pub fn playing(&self) -> bool {
        self.guest_on
    }

    /// One guest period has come in: its lateness against the guest's schedule (the earliest arrival of the last one
    /// to two seconds less its sample time) plus what the ring and AAudio hold ahead of it (`ahead`, frames; None: no
    /// timestamp yet) is its end to end, from iOS's sample time to the speaker.
    fn guest_period(&mut self, frames: u32, ahead: Option<i64>) {
        let now = time::seconds(libc::CLOCK_MONOTONIC_RAW);
        let c = &mut self.clock;
        let at = c.frames as f64 / f64::from(RATE);
        let offset = now - at;
        if c.since < 0.0 || now - c.since >= 1.0 {
            c.last_min = if c.since < 0.0 || now - c.since >= 2.0 { 1e30 } else { c.now_min };
            c.now_min = offset;
            c.since = now;
        } else if offset < c.now_min {
            c.now_min = offset;
        }
        c.frames += u64::from(frames);
        let late = offset - c.now_min.min(c.last_min);
        if late > self.window_late_max {
            self.window_late_max = late; // what the target may come down to (the writer)
        }
        self.window_periods += 1;
        let Some(ahead) = ahead else { return };
        let e2e = late + ahead as f64 / f64::from(RATE);
        self.stats.e2e_sum += e2e;
        self.stats.late_sum += late;
        self.stats.e2e_n += 1;
        if e2e > self.stats.e2e_max {
            self.stats.e2e_max = e2e;
        }
    }

    fn silent(&self, at: u32, frames: u32) -> bool {
        (0..frames).all(|i| {
            let f = 2 * ((at + i) % RING_FRAMES) as usize;
            (-1..=1).contains(&self.ring[f]) && (-1..=1).contains(&self.ring[f + 1])
        })
    }

    /// The target up by 5 ms (an underrun, AAudio's xrun), the level measured anew. One raise per guest period
    /// (CHUNK, 5 ms) however many reports come in it: AAudio's MMAP xrun counter goes up on each write attempt while
    /// its read is ahead of the write (18 increments in 0.3 ms on an SM8850, YMGPwcca/OneiOS c4d9e318), one starvation
    /// and not 18 periods lost; raised for each, the level went to its ceiling (80 ms) at once and stayed there as
    /// latency. The raw counts (stats.xruns, stats.underruns) still count every report. False: coalesced.
    pub fn raise_target(&mut self, now: f64) -> bool {
        if self.raised_at >= 0.0 && now - self.raised_at < f64::from(CHUNK) / f64::from(RATE) {
            return false;
        }
        self.raised_at = now;
        self.target = (self.target + ms_frames(5)).min(ms_frames(self.target_max_ms));
        if self.target > self.stats.target_peak {
            self.stats.target_peak = self.target;
        }
        self.window_late_max = 0.0;
        self.window_periods = 0;
        true
    }

    fn restart(&mut self) {
        self.guest_on = true;
        self.raised_at = -1.0;
        self.generation += 1;
        self.dead = false;
        self.stats = OutStats::default();
        self.clock = GuestClock::default();
    }
}

/// One stream and what its thread and the main loop share.
pub struct Dir {
    stream: Arc<dyn AStream>,
    burst: u32,
    channels: u32,
    run: AtomicBool,
    /// written (output) or read (input) through AAudio
    frames: AtomicU64,
    started: Mutex<f64>,
}

impl Dir {
    fn started(&self) -> f64 {
        *self.started.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Frames written but not yet played by the DAC, from AAudio's timestamp (None: no timestamp yet).
    fn pipeline_frames(&self) -> Option<i64> {
        let (position, when) = self.stream.timestamp()?;
        let played = position + (now_ns() - when) * i64::from(RATE) / 1_000_000_000;
        Some((self.frames.load(Ordering::Relaxed) as i64 - played).max(0))
    }

    /// What the server has read (the DSP, the endpoint or the mixer) and not yet presented: the HAL and the DSP after
    /// the read (frames; None: no timestamp yet).
    fn below_read(&self) -> Option<i64> {
        let (position, when) = self.stream.timestamp()?;
        let below = self.stream.frames_read() - (position + (now_ns() - when) * i64::from(RATE) / 1_000_000_000);
        Some(below.max(0))
    }
}

/// One UI's media volume on the speaker, as the helper found and changed it.
#[derive(Clone, Copy, Debug, Default)]
struct HostVolume {
    tried: bool,
    changed: bool,
    found: i32,
    max: i32,
}

/// The helper: its socket, AAudio, the two directions and the ring.
pub struct Helper {
    sock: OwnedFd,
    aaudio: Arc<dyn AAudio>,
    pub settings: Settings,
    ring: Mutex<Ring>,
    ring_more: Condvar,
    out: Mutex<Option<Arc<Dir>>>,
    inp: Mutex<Option<Arc<Dir>>>,
    out_thread: Mutex<Option<JoinHandle<()>>>,
    in_thread: Mutex<Option<JoinHandle<()>>>,
    /// AAudio_setMMapPolicy(NEVER) worked
    mmap_refused: AtomicBool,
    /// a stream's server took no frames (no timestamp, 600 ms of timed-out writes): no open asks for MMAP again
    mmap_dead: AtomicBool,
    /// the output latency iOS was told (AU_TOLD; 0: not known)
    told_frames: AtomicU32,
    host_volume: Mutex<HostVolume>,
    idle_since: Mutex<f64>,
    in_sent: AtomicU64,
    in_lost: AtomicU64,
    in_short: AtomicU64,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// No realtime threads: every thread of the process found SCHED_FIFO or SCHED_RR is set back to SCHED_OTHER (logged).
/// SCHED_OTHER (0; Android's libc crate does not name it).
const SCHED_OTHER: i32 = 0;

fn demote_realtime() -> u32 {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else { return 0 };
    let mut demoted = 0;
    for e in tasks.flatten() {
        let Ok(tid) = e.file_name().to_string_lossy().parse::<libc::pid_t>() else { continue };
        if tid <= 0 {
            continue;
        }
        // SAFETY: a query of a thread's policy.
        let policy = unsafe { libc::sched_getscheduler(tid) };
        if policy < 0 {
            continue;
        }
        let policy = policy & !libc::SCHED_RESET_ON_FORK;
        if policy != libc::SCHED_FIFO && policy != libc::SCHED_RR {
            continue;
        }
        let normal = libc::sched_param { sched_priority: 0 };
        // SAFETY: a valid sched_param.
        let rc = unsafe { libc::sched_setscheduler(tid, SCHED_OTHER, &normal) };
        eprintln!(
            "oneios-audio: thread {tid} was {}: {}",
            if policy == libc::SCHED_FIFO { "SCHED_FIFO" } else { "SCHED_RR" },
            if rc != 0 { strerror(errno()) } else { "set back to SCHED_OTHER".to_owned() }
        );
        demoted += 1;
    }
    demoted
}

/// Whether system_server runs (its scheduling_policy service answers an MMAP shared endpoint's thread registration).
/// ONEIOS_AUDIO_SYSTEM_SERVER=0|1 says so instead (the host test).
fn system_server_up() -> bool {
    if let Ok(forced) = std::env::var("ONEIOS_AUDIO_SYSTEM_SERVER") {
        if !forced.is_empty() {
            return forced.starts_with('1');
        }
    }
    let Ok(proc) = std::fs::read_dir("/proc") else { return false };
    proc.flatten().any(|e| {
        let name = e.file_name();
        let name = name.to_string_lossy();
        name.starts_with(|c: char| ('1'..='9').contains(&c))
            && std::fs::read(format!("/proc/{name}/comm")).is_ok_and(|comm| comm.starts_with(b"system_server\n"))
    })
}

/// AAudio's advice for an output stream: its buffer filled before the asynchronous START (probe_prefill). Non-blocking
/// writes of silence up to the buffer size; false (logged) when the buffer is unusable or a write comes back short:
/// the stream is then not started.
fn prefill_speaker(stream: &dyn AStream) -> bool {
    let (size, capacity) = (stream.buffer_size(), stream.buffer_capacity());
    if size <= 0 || capacity <= 0 || size > capacity || size > RING_FRAMES as i32 {
        eprintln!("oneios-audio: speaker prefill: invalid buffer {size} of {capacity} frames");
        return false;
    }
    let zeros = [0i16; CHUNK as usize * 2];
    let mut filled = 0i32;
    while filled < size {
        let wanted = (size - filled).min(CHUNK as i32);
        let n = stream.write(&zeros, wanted, 0);
        if n <= 0 || n > wanted {
            eprintln!("oneios-audio: speaker prefill: {filled}/{size} frames; write returned {n}");
            return false;
        }
        filled += n;
    }
    eprintln!("oneios-audio: speaker prefill: {filled}/{size} frames before requestStart");
    true
}

/// The latest burst size a stream reported as the writer's: 16..MAX_BURST, else 240.
fn usable_burst(burst: i32) -> u32 {
    if (16..=MAX_BURST as i32).contains(&burst) {
        burst as u32
    } else {
        240
    }
}

/// One burst of output from the ring: `want` frames played from `take` frames (take = want, or one more or fewer to
/// steer the fill toward the target), linear interpolation between them. A short chunk (an underrun, a sound's tail)
/// is padded with silence.
pub fn resample(out: &mut [i16], want: u32, input: &[i16], take: u32) {
    let (w, t) = (want as usize, take as usize);
    if take == want || take < 2 || want < 2 || take + 4 < want || take > want + 4 {
        let n = t.min(w);
        out[..2 * n].copy_from_slice(&input[..2 * n]);
        out[2 * n..2 * w].fill(0);
        return;
    }
    for i in 0..w {
        let pos = i as u64 * (t as u64 - 1) * 65536 / (w as u64 - 1);
        let a = (pos >> 16) as usize;
        let b = if a + 1 < t { a + 1 } else { a };
        let f = (pos & 0xffff) as i32;
        for c in 0..2 {
            out[2 * i + c] =
                ((i32::from(input[2 * a + c]) * (65536 - f) + i32::from(input[2 * b + c]) * f) >> 16) as i16;
        }
    }
}

impl Helper {
    /// The helper on socket `sock` (its end of xnuhost's pair) with `aaudio`; the settings from the environment.
    pub fn new(sock: OwnedFd, aaudio: Arc<dyn AAudio>) -> Arc<Helper> {
        Arc::new(Helper {
            sock,
            aaudio,
            settings: Settings::from_env(),
            ring: Mutex::new(Ring::new()),
            ring_more: Condvar::new(),
            out: Mutex::new(None),
            inp: Mutex::new(None),
            out_thread: Mutex::new(None),
            in_thread: Mutex::new(None),
            mmap_refused: AtomicBool::new(false),
            mmap_dead: AtomicBool::new(false),
            told_frames: AtomicU32::new(0),
            host_volume: Mutex::new(HostVolume::default()),
            idle_since: Mutex::new(0.0),
            in_sent: AtomicU64::new(0),
            in_lost: AtomicU64::new(0),
            in_short: AtomicU64::new(0),
        })
    }

    /// The ring and what the writer shares (tests read it).
    pub fn ring(&self) -> MutexGuard<'_, Ring> {
        locked(&self.ring)
    }

    /// Whether the speaker's and the microphone's streams are open.
    pub fn streams_open(&self) -> (bool, bool) {
        (locked(&self.out).is_some(), locked(&self.inp).is_some())
    }

    /// The output latency iOS was told (frames; 0: not known).
    pub fn told_frames(&self) -> u32 {
        self.told_frames.load(Ordering::Relaxed)
    }

    fn dir(&self, output: bool) -> Option<Arc<Dir>> {
        locked(if output { &self.out } else { &self.inp }).clone()
    }

    fn send(&self, bytes: &[u8]) -> isize {
        // SAFETY: a valid buffer of its length.
        unsafe {
            libc::send(
                self.sock.as_raw_fd(),
                bytes.as_ptr().cast(),
                bytes.len(),
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        }
    }

    fn report(&self, output: bool, error: i32) {
        // Never block: xnuhost reads its end only while the microphone runs (backend.rs), and a full socket would stop
        // this process's receive loop.
        let header = AuHeader { kind: AU_STATE, output: output.into(), value: error as u32, ..Default::default() };
        let _ = self.send(&header.to_bytes());
    }

    // ---- One UI's media volume on the speaker (fix-audio-volume, 2026-11-22) --------------------------------------
    // The speaker stream is usage MEDIA, so AudioFlinger plays it at One UI's media volume of the speaker: the index the
    // audio policy had when system_server stopped (nothing changes it in a session). That was the "low volume" of the
    // 2026-11-22 report, and iOS's own volume could not change anything. iOS's volume is now applied in the guest, so
    // the host plays at full scale: at the speaker's first open the helper sets the audio policy's index of USAGE_MEDIA
    // on the speaker to its maximum, through libaudioclient's AudioSystem (only the policy's live index changes, One
    // UI's saved volume is in system_server's settings and is not touched), and sets the index it found back when it
    // ends. [I] When the return kills the namespace instead, system_server's AudioService sets every saved index again
    // as it starts. The functions are looked up by their mangled names, which carry the parameter types, so a build
    // whose signature differs is not called. ONEIOS_AUDIO_HOST_VOLUME=keep: not touched (the path before 2026-11-22).

    /// At the speaker's first open of this run: One UI's media index on the speaker to its maximum.
    fn host_volume_full(&self) {
        let mut hv = locked(&self.host_volume);
        if hv.tried {
            return;
        }
        hv.tried = true;
        if std::env::var("ONEIOS_AUDIO_HOST_VOLUME").is_ok_and(|v| v == "keep") {
            eprintln!(
                "oneios-audio: host volume: kept (ONEIOS_AUDIO_HOST_VOLUME=keep): the speaker plays at One UI's media \
                 volume"
            );
            return;
        }
        if system_server_up() {
            // beside One UI (bridgectl): its volume is AudioService's
            eprintln!(
                "oneios-audio: host volume: kept: system_server runs (One UI's media volume is its AudioService's)"
            );
            return;
        }
        let calls = self.aaudio.volume_calls();
        if !calls.get || !calls.max || (!calls.set && !calls.set_muted) {
            eprintln!(
                "oneios-audio: host volume: libaudioclient's AudioSystem volume calls not found (get {}, max {}, set \
                 {}/{}): the speaker plays at One UI's media volume",
                u8::from(calls.get),
                u8::from(calls.max),
                u8::from(calls.set),
                u8::from(calls.set_muted)
            );
            return;
        }
        let ((rc_get, index), (rc_max, max)) = (self.aaudio.volume_index(), self.aaudio.volume_max());
        if rc_get != 0 || rc_max != 0 || index < 0 || max <= 0 {
            eprintln!(
                "oneios-audio: host volume: the audio policy did not say One UI's media volume (index {index} status \
                 {rc_get}, maximum {max} status {rc_max}): not changed"
            );
            return;
        }
        hv.found = index;
        hv.max = max;
        if index >= max {
            eprintln!(
                "oneios-audio: host volume: One UI's media volume on the speaker is at its maximum (index {index}): the \
                 stream plays at full scale, iOS's volume is the one that counts"
            );
            return;
        }
        let rc = self.aaudio.set_volume_index(max);
        hv.changed = rc == 0;
        eprintln!(
            "oneios-audio: host volume: One UI's media volume on the speaker was index {index} of {max}; {} (One UI's \
             saved volume is not changed; the index is put back when the helper ends)",
            if rc != 0 {
                "set to the maximum refused"
            } else {
                "set to the maximum for the session: the stream plays at full scale, iOS's volume is the one that counts"
            }
        );
        if rc != 0 {
            eprintln!("oneios-audio: host volume: setVolumeIndexForAttributes(media, {max}, speaker): status {rc}");
        }
    }

    /// When the helper ends: the index it found back.
    fn host_volume_restore(&self) {
        let mut hv = locked(&self.host_volume);
        if hv.changed {
            let rc = self.aaudio.set_volume_index(hv.found);
            eprintln!(
                "oneios-audio: host volume: One UI's media volume on the speaker put back to index {} of {} (status {rc})",
                hv.found, hv.max
            );
        }
        *hv = HostVolume::default();
    }

    /// AAudio_setMMapPolicy (process-wide, read at each open): whether it took.
    fn mmap_policy(&self, policy: i32) -> bool {
        let Some(rc) = self.aaudio.set_mmap_policy(policy) else { return false };
        if rc != AAUDIO_OK {
            eprintln!(
                "oneios-audio: AAudio_setMMapPolicy({}): {}",
                if policy == POLICY_NEVER { "NEVER" } else { "AUTO" },
                self.aaudio.result_text(rc)
            );
        }
        rc == AAUDIO_OK
    }

    fn mmap_off(&self) {
        let refused = self.mmap_policy(POLICY_NEVER);
        self.mmap_refused.store(refused, Ordering::Relaxed);
        if !refused {
            eprintln!("oneios-audio: MMAP not refused: a stream that is not MMAP exclusive gets performance mode none");
        }
    }

    /// The writer thread's boost: uclamp.min with its policy and nice kept (SCHED_OTHER).
    fn boost_writer(&self) {
        let u = self.settings.uclamp_min;
        if u == 0 {
            return;
        }
        if let Err(e) = crate::cpu::set_uclamp_min(0, u) {
            eprintln!("oneios-audio: speaker writer: uclamp.min {u}: {}", xh_core::sys::error_text(&e));
        }
    }

    fn open_with(&self, output: bool, rate: u32, channels: u32, perf: i32, sharing: i32) -> Option<Arc<dyn AStream>> {
        match self.aaudio.open(&StreamRequest { output, rate, channels, perf, sharing }) {
            Ok(s) => Some(s),
            Err(rc) => {
                eprintln!(
                    "oneios-audio: open {}: {}",
                    if output { "speaker" } else { "microphone" },
                    self.aaudio.result_text(rc)
                );
                None
            }
        }
    }

    /// The speaker's stream on the first path given (the header's 1, 2, 3); sets the ring's out_path. Also what was
    /// asked and what was passed over (`why`).
    fn open_speaker(&self, rate: u32, channels: u32) -> (Option<Arc<dyn AStream>>, i32, String) {
        let s = &self.settings;
        let mut path = OutPath::Legacy;
        let mut why = String::new();
        let mut asked;
        if self.mmap_dead.load(Ordering::Relaxed) {
            why = "an earlier MMAP stream's server took no frames".to_owned();
        } else if s.perf_mode == PERF_LOW_LATENCY && s.mmap_want != MmapWant::Off {
            // Without AAudio_setMMapPolicy the system's aaudio.mmap_policy decides (2, AUTO, on the phone).
            let _ = self.mmap_policy(POLICY_AUTO);
            self.mmap_refused.store(false, Ordering::Relaxed);
            asked = PERF_LOW_LATENCY;
            let sharing = if s.mmap_want == MmapWant::Exclusive { SHARING_EXCLUSIVE } else { SHARING_SHARED };
            let mut stream = self.open_with(true, rate, channels, asked, sharing);
            match &stream {
                None => why = "LOW_LATENCY with MMAP did not open".to_owned(),
                Some(st) => {
                    let used = st.mmap_used();
                    let exclusive = st.sharing_mode() == SHARING_EXCLUSIVE; // a legacy track is never exclusive
                    if exclusive && used != 0 {
                        path = OutPath::MmapExclusive;
                    } else if used == 1 && system_server_up() {
                        path = OutPath::MmapShared;
                    } else if used == 0 {
                        why = "MMAP not given".to_owned(); // AAudio's own legacy track: kept
                    } else {
                        why = if used == 1 {
                            "MMAP shared without system_server (its endpoint thread would wait for scheduling_policy \
                             forever): closed before its start"
                        } else {
                            "MMAP use not known and not exclusive: closed"
                        }
                        .to_owned();
                        st.close();
                        stream = None;
                    }
                }
            }
            if stream.is_some() {
                self.ring().out_path = path;
                return (stream, asked, why);
            }
        } else {
            if why.is_empty() {
                why = if s.perf_mode == PERF_LOW_LATENCY { "ONEIOS_AUDIO_MMAP=off" } else { "ONEIOS_AUDIO_PERF=none" }
                    .to_owned();
            }
        }
        self.mmap_off();
        asked = if self.mmap_refused.load(Ordering::Relaxed) { s.perf_mode } else { PERF_NONE };
        let mut stream = self.open_with(true, rate, channels, asked, SHARING_SHARED);
        if let Some(st) = &stream {
            if asked != PERF_NONE && st.mmap_used() == 1 {
                eprintln!(
                    "oneios-audio: speaker: MMAP in use despite the policy: opened again with performance mode none"
                );
                st.close();
                asked = PERF_NONE;
                stream = self.open_with(true, rate, channels, asked, SHARING_SHARED);
            }
        }
        self.ring().out_path = OutPath::Legacy;
        (stream, asked, why)
    }

    fn open_stream(&self, output: bool, rate: u32, channels: u32, prefill: bool) -> Option<Arc<dyn AStream>> {
        // The speaker on its path; the microphone performance mode NONE, shared (AAudio tries MMAP only for LOW_LATENCY).
        let (stream, asked, why) = if output {
            self.open_speaker(rate, channels)
        } else {
            (self.open_with(false, rate, channels, PERF_NONE, SHARING_SHARED), PERF_NONE, String::new())
        };
        let stream = stream?;
        let burst = stream.frames_per_burst();
        let (out_path, target_ms) = if output {
            // The level's floor: 5 ms above what an MMAP reader takes before the guest's next period, 20 ms above a
            // mixer's next pull (ONEIOS_AUDIO_TARGET_MS sets it for both). The DSP may take a burst at once: on MMAP
            // exclusive the floor is at least a burst.
            let mut r = self.ring();
            r.continuous = r.out_path != OutPath::Legacy;
            let burst_ms = if burst > 0 { (burst as u32 * 1000).div_ceil(RATE) } else { 0 };
            r.target_ms = if self.settings.target_env_ms != 0 {
                self.settings.target_env_ms
            } else if !r.continuous {
                20
            } else if r.out_path == OutPath::MmapExclusive && burst_ms > 5 {
                burst_ms
            } else {
                5
            };
            r.target_max_ms = r.target_ms.max(80);
            (r.out_path, r.target_ms)
        } else {
            (OutPath::Legacy, 0)
        };
        // AAudio's own buffer: ONEIOS_AUDIO_BURSTS bursts, or (by default) a burst plus the target plus a chunk, as the
        // writer keeps it (buffer_fit).
        if output && burst > 0 {
            let frames = if self.settings.buffer_bursts != 0 {
                burst * self.settings.buffer_bursts as i32
            } else {
                burst + (ms_frames(target_ms) + CHUNK) as i32
            };
            let capacity = stream.buffer_capacity();
            stream.set_buffer_size(if capacity > 0 && frames > capacity { capacity } else { frames });
        }
        eprintln!(
            "oneios-audio: {} open: {} Hz, {} channel(s), {} asked, {} given, sharing {}, mmap {}, burst {burst}, buffer {} \
             of {} frames, device {}, session {}{}{}{}{}{}",
            if output { "speaker" } else { "microphone" },
            stream.sample_rate(),
            stream.channel_count(),
            perf_name(asked),
            perf_name(stream.performance_mode()),
            if stream.sharing_mode() == SHARING_EXCLUSIVE { "exclusive" } else { "shared" },
            stream.mmap_used(),
            stream.buffer_size(),
            stream.buffer_capacity(),
            stream.device_id(),
            stream.session_id(),
            if output { "; " } else { "" },
            if output { path_name(out_path) } else { "" },
            if why.is_empty() { "" } else { " (" },
            why,
            if why.is_empty() { "" } else { ")" }
        );
        // AAudio's START is asynchronous; a HAL that needs its buffer full first (probe_prefill) gets it before it.
        if output && prefill && !prefill_speaker(&*stream) {
            stream.close();
            return None;
        }
        let rc = stream.request_start();
        if rc != AAUDIO_OK {
            eprintln!(
                "oneios-audio: start {}: {}",
                if output { "speaker" } else { "microphone" },
                self.aaudio.result_text(rc)
            );
            stream.close();
            return None;
        }
        Some(stream)
    }

    /// The speaker's report line ("running", "stopped", "stopped by the guest").
    fn out_report(&self, when: &str) {
        let (s, fill, aim, pull, period, continuous, out_path) = {
            let mut r = self.ring();
            let s = r.stats;
            r.stats.fill_min = r.count;
            r.stats.fill_max = r.count;
            r.stats.e2e_sum = 0.0; // the end to end is per report
            r.stats.e2e_max = 0.0;
            r.stats.late_sum = 0.0;
            r.stats.e2e_n = 0;
            (s, r.count, r.target, r.pulls.size, r.pulls.period, r.continuous, r.out_path)
        };
        let dir = self.dir(true);
        let queued = dir.as_ref().and_then(|d| d.pipeline_frames());
        let xruns = dir.as_ref().map_or(0, |d| d.stream.xrun_count());
        let pulled = if continuous {
            format!(
                "the reader takes frames all the time ({})",
                if out_path == OutPath::MmapExclusive { "MMAP exclusive" } else { "MMAP shared" }
            )
        } else if pull != 0 {
            format!("server pulls {pull} frames every {:.1} ms", period * 1e3)
        } else {
            "server pulls not seen".to_owned()
        };
        let told = match self.told_frames() {
            0 => "not known".to_owned(),
            t => format!("{} ms", frames_ms(t)),
        };
        let e2e = if s.e2e_n != 0 {
            let n = f64::from(s.e2e_n);
            format!(
                "end-to-end {:.0} ms (max {:.0}; the guest's period late by {:.1} ms + the host {:.0} ms; iOS is told {told})",
                s.e2e_sum / n * 1e3,
                s.e2e_max * 1e3,
                s.late_sum / n * 1e3,
                (s.e2e_sum - s.late_sum) / n * 1e3
            )
        } else {
            format!("end-to-end not measured (no guest period with a timestamp; iOS is told {told})")
        };
        let started = dir.as_ref().map_or(time::monotonic_s(), |d| d.started());
        let ms_of = |q: Option<i64>| q.map_or(-1, |q| frames_ms(q as u32) as i64);
        eprintln!(
            "oneios-audio: speaker {when}: {:.1} s, {} frames in, {} played, {} underruns, {} overflows ({} frames \
             dropped), slips +{}/-{}, {} frames of silence cut, ring {} ms ({}..{}, target {}, peak {}), margin {} ms, \
             {pulled}, AAudio {} ms queued, {xruns} xruns; latency about {} ms; {e2e}",
            time::monotonic_s() - started,
            s.r#in,
            s.played,
            s.underruns,
            s.overflows,
            s.dropped,
            s.slips_up,
            s.slips_down,
            s.silence,
            frames_ms(fill),
            frames_ms(s.fill_min),
            frames_ms(s.fill_max),
            frames_ms(aim),
            frames_ms(s.target_peak.max(aim)),
            s.margin * 1000 / RATE as i32,
            ms_of(queued),
            ms_of(queued.map(|q| q + i64::from(fill)))
        );
    }

    /// AAudio's buffer size is AudioFlinger's start threshold (a new track is not read until it holds that much) and
    /// the most a blocking write leaves in it. Fitted to the most the level needs (a burst, which is at least a pull,
    /// plus the target) and a chunk: the start then adds no more than that chunk, and a backlog waits in the ring,
    /// where its silence can be cut. Set again when the target moves; ONEIOS_AUDIO_BURSTS fixes it instead.
    fn buffer_fit(&self, d: &Dir, aim: u32) {
        if self.settings.buffer_bursts != 0 {
            return;
        }
        let capacity = d.stream.buffer_capacity();
        let frames = (d.burst + aim + CHUNK) as i32;
        d.stream.set_buffer_size(if capacity > 0 && frames > capacity { capacity } else { frames });
    }

    /// The writer moves CHUNK frames at a time from the ring to AAudio (a blocking write: the server paces it). It
    /// waits while the ring has less than a chunk and AAudio still holds what its next pull needs; only when AAudio is
    /// about to run dry does it play what the ring has and silence for the rest (an underrun), and prime again.
    /// Priming (and while the guest is stopped): silence keeps AAudio's level at the need (the pull's part plus the
    /// target); the guest's audio starts on that level, so the pull after it still finds a margin of the target.
    fn writer(self: Arc<Helper>, d: Arc<Dir>) {
        let mut chunk = vec![0i16; (CHUNK as usize + 4) * 2];
        let mut from = vec![0i16; (CHUNK as usize + 4) * 2];
        let low_blind = i64::from(d.burst / 2);
        // the level (less the pull's part), averaged over about 200 chunks (1 s)
        let (mut level, mut reference) = (-1.0f64, -1.0f64);
        let (mut settled, mut seen) = (0u32, 0u32);
        // quiet_since: the last underrun or target change
        let (mut ring_average, mut quiet_since) = (0.0f64, time::monotonic_s());
        let mut phase = false;
        let mut xruns_seen = d.stream.xrun_count();
        let mut reported = d.started();
        // The server's read and when it last moved: a stream AAudio holds frames for and that does not take them for a
        // second is dead (an MMAP stream whose DSP never started, or stopped), and the receive loop replaces it.
        let mut read_seen = d.stream.frames_read();
        let mut read_moved = time::monotonic_s();
        // writes in a row the server took nothing of (200 ms each): its DSP never started
        let mut idle = 0u32;
        self.boost_writer();
        let mut fitted = {
            let mut r = self.ring();
            r.pulls = Pulls::reset(d.stream.frames_read());
            r.target
        };
        self.buffer_fit(&d, fitted);
        while d.run.load(Ordering::Acquire) {
            let read = d.stream.frames_read();
            let queued = (d.stream.frames_written() - read).max(0);
            let xruns = d.stream.xrun_count();
            let now = time::monotonic_s();
            // Its DSP never started (no timestamp; this phone's HAL does it now and then) or stopped: AAudio holds
            // frames and takes none for a second. The receive loop reopens the speaker on the legacy path, once.
            if read != read_seen {
                read_seen = read;
                read_moved = now;
            } else if queued > 0 && now - read_moved >= 1.0 {
                let mut r = self.ring();
                if r.out_path != OutPath::Legacy {
                    r.dead = true;
                    drop(r);
                    eprintln!(
                        "oneios-audio: speaker: the server took no frames in 1 s: the speaker is reopened as a mixer \
                         track"
                    );
                    break;
                }
            }
            let (mut take, mut length) = (0u32, CHUNK);
            let (mut play, mut cut) = (false, false);
            let mut r = self.ring();
            r.pulls.saw(read, now);
            // An MMAP reader (the DSP, or AAudio's endpoint every burst) takes frames all the time: what it takes
            // before the guest's next period comes in is a chunk, whatever the phase. A mixer takes a period at a time
            // (its pulls). AAudio's endpoint (MMAP shared) takes a burst at a time on top of that.
            let ahead: Option<i64> = if r.continuous {
                Some(i64::from(CHUNK) + if r.out_path == OutPath::MmapShared { i64::from(d.burst) } else { 0 })
            } else {
                r.pulls.ahead(now)
            };
            if xruns > xruns_seen {
                // AAudio ran dry (the writer was late): the level goes up
                r.stats.xruns += (xruns - xruns_seen) as u64;
                r.raise_target(now);
                quiet_since = now;
                (level, reference, settled) = (-1.0, -1.0, 0);
            }
            xruns_seen = xruns;
            if !r.continuous && ahead.is_some() && !r.pulls.told {
                r.pulls.told = true;
                eprintln!(
                    "oneios-audio: speaker: the server pulls {} frames every {:.1} ms: the level follows its pulls",
                    r.pulls.size,
                    r.pulls.period * 1e3
                );
            }
            if ahead.is_some() != phase {
                phase = ahead.is_some();
                (level, reference, settled) = (-1.0, -1.0, 0);
            }
            let ahead_v = ahead.unwrap_or(-1);
            let taken = read > 0;
            if taken && !r.consuming {
                r.allowance = r.count; // the stall is over: what it left is the allowance
            }
            r.consuming = taken;
            if r.consuming {
                let floor = r.target + ms_frames(60);
                let keep = r.count.max(floor);
                if keep < r.allowance {
                    r.allowance = keep;
                }
            }
            if seen != r.generation {
                seen = r.generation;
                (level, reference, settled, ring_average) = (-1.0, -1.0, 0, 0.0);
            }
            // The target back toward its floor after a quiet while. On MMAP the target is the slack the guest's periods
            // have before the reader runs dry (the target less the 2 ms of `low`): it comes down only when the latest
            // guest period of the quiet while came within that slack less 1 ms (on the CI runner, a VM, the guest's
            // periods were late by 3 ms at times, and a 5 ms target then cost a gap of silence in 9 s).
            let decay_s = f64::from(self.settings.target_decay_s);
            if r.target > ms_frames(r.target_ms) && now - quiet_since >= decay_s {
                let lowered = if r.target > ms_frames(r.target_ms + 5) {
                    r.target - ms_frames(5)
                } else {
                    ms_frames(r.target_ms)
                };
                // (a quiet while with no guest period says nothing: the target stays.)
                if !r.continuous
                    || (r.window_periods != 0 && r.window_late_max * 1e3 + 3.0 < f64::from(frames_ms(lowered)))
                {
                    r.target = lowered;
                    (level, reference, settled) = (-1.0, -1.0, 0);
                    eprintln!(
                        "oneios-audio: speaker: target {} ms ({} s without an underrun)",
                        frames_ms(r.target),
                        self.settings.target_decay_s
                    );
                }
                quiet_since = now;
                r.window_late_max = 0.0;
                r.window_periods = 0;
            }
            let target = i64::from(r.target);
            let burst = i64::from(d.burst);
            // The need: what the next pull takes from what came in since the last one, plus the target. Blind: a burst
            // plus the target, at every moment. An MMAP reader: what it takes beyond the chunk a guest period brings
            // (none for the DSP, a burst for AAudio's endpoint, which takes one at a time): priming to the whole of
            // `ahead` left the level a chunk high when the guest's first period came in.
            let need = target
                + if r.continuous {
                    ahead_v - i64::from(CHUNK)
                } else if phase {
                    ahead_v
                } else {
                    burst
                };
            // About to run dry: below what the next pull takes; for AAudio's endpoint below its burst; for the DSP below
            // 2 ms (the writer's wait and a write).
            let low = if r.out_path == OutPath::MmapShared {
                burst
            } else if r.continuous {
                i64::from(ms_frames(2))
            } else if phase {
                ahead_v
            } else {
                low_blind
            };
            let count = i64::from(r.count);
            let margin = count + queued - if phase { ahead_v } else { burst };
            r.stats.margin = if level >= 0.0 && phase { level as i32 } else { margin as i32 };
            if !r.priming && r.count >= CHUNK && margin > target + i64::from(ms_frames(10)) && r.silent(r.head, CHUNK) {
                cut = true; // a backlog's silence: dropped, the level comes down at once
                r.head = (r.head + CHUNK) % RING_FRAMES;
                r.count -= CHUNK;
                r.stats.silence += u64::from(CHUNK);
                if level >= 0.0 {
                    level -= f64::from(CHUNK);
                }
                ring_average -= f64::from(CHUNK);
            } else if r.priming {
                // Before the guest's first chunk (and while it is stopped) AAudio is kept at the need with silence: a
                // track left empty is disabled by the mixer, and the level the guest's audio starts on is that. An MMAP
                // reader takes frames all the time: the guest's first chunk goes in only on top of the whole need in
                // AAudio (counting the chunk let it start on whatever the reader had left of the silence, up to a burst
                // under the target, and the first gap between periods ran dry: one underrun at each stream's start).
                if r.count >= CHUNK && (if r.continuous { 0 } else { count }) + queued >= need {
                    r.priming = false;
                } else if queued < need {
                    // silence, as much as is missing
                    play = true;
                    let missing = need - queued;
                    length = if missing >= i64::from(CHUNK) {
                        CHUNK
                    } else if missing < 16 {
                        16
                    } else {
                        missing as u32
                    };
                }
            }
            if play || r.priming || cut {
            } else if r.count >= CHUNK {
                play = true;
                let now_level = margin as f64;
                level = if level < 0.0 { now_level } else { level + (now_level - level) / 200.0 };
                ring_average += (f64::from(r.count) - ring_average) / 200.0;
                take = CHUNK;
                let t = f64::from(r.target);
                // A slip: the chunk's 240 ring frames are played as 239 or 237 (the level comes down by 1 or 3) or as
                // 241 (up).
                if phase {
                    // The level's aim is the target above what the next pull takes: one frame fewer per chunk while it
                    // is 5 ms above (drift), three while 10 ms above (a start: the track's start threshold and its first
                    // pull's phase leave 20-40 ms; a backlog), one more while 5 ms below.
                    if level > t + f64::from(ms_frames(10)) {
                        length = CHUNK - 3;
                        r.stats.slips_down += 3;
                        level -= 3.0;
                    } else if level > t + f64::from(ms_frames(5)) {
                        length = CHUNK - 1;
                        r.stats.slips_down += 1;
                        level -= 1.0;
                    // An MMAP reader's target is only a few ms: a level 1 ms under it already comes up (a 5 ms band
                    // would let it sit at nothing).
                    } else if level + f64::from(if r.continuous { ms_frames(1) } else { ms_frames(5) }) < t {
                        length = CHUNK + 1;
                        r.stats.slips_up += 1;
                        level += 1.0;
                    }
                } else {
                    // Blind (the pulls not seen yet): the level's reference is its value 1 s after the start; fewer
                    // frames while the ring holds more than it needs (after a stall or a burst of late periods: that is
                    // latency), three fewer while it holds 40 ms more.
                    if reference < 0.0 {
                        settled += 1;
                        if settled == 200 {
                            reference = level;
                        }
                    }
                    if ring_average > t + f64::from(ms_frames(40)) {
                        length = CHUNK - 3;
                        r.stats.slips_down += 3;
                        level -= 3.0;
                        ring_average -= 3.0;
                    } else if (reference >= 0.0 && level > reference + f64::from(ms_frames(5)))
                        || ring_average > t + f64::from(ms_frames(10))
                    {
                        length = CHUNK - 1;
                        r.stats.slips_down += 1;
                        level -= 1.0;
                        ring_average -= 1.0;
                    } else if reference >= 0.0 && level + f64::from(ms_frames(5)) < reference {
                        length = CHUNK + 1;
                        r.stats.slips_up += 1;
                        level += 1.0;
                    }
                }
            } else if !r.guest_on {
                // the guest stopped: its tail, then silence
                play = true;
                take = r.count;
                if take == 0 {
                    r.priming = true;
                }
            } else if queued < low {
                // AAudio is about to run dry
                play = true;
                take = r.count;
                r.priming = true;
                r.stats.underruns += 1;
                r.raise_target(now);
                quiet_since = now;
                (level, reference, settled) = (-1.0, -1.0, 0);
            }
            for i in 0..take as usize {
                let at = 2 * r.head as usize;
                from[2 * i] = r.ring[at];
                from[2 * i + 1] = r.ring[at + 1];
                r.head = (r.head + 1) % RING_FRAMES;
            }
            r.count -= take;
            r.stats.played += u64::from(take);
            if play && r.count < r.stats.fill_min {
                r.stats.fill_min = r.count;
            }
            if !play && !cut {
                // nothing to do: wait for the guest's next period (or 2 ms)
                r = self.ring_more.wait_timeout(r, Duration::from_millis(2)).unwrap_or_else(PoisonError::into_inner).0;
            }
            let aim = r.target;
            drop(r);
            if aim != fitted {
                fitted = aim;
                self.buffer_fit(&d, aim);
            }
            if play {
                resample(&mut chunk, length, &from, take);
                let mut done = 0u32;
                while done < length && d.run.load(Ordering::Acquire) {
                    let n = d.stream.write(&chunk[2 * done as usize..], (length - done) as i32, 200_000_000); // 200 ms
                    if n < 0 {
                        eprintln!("oneios-audio: speaker write: {}", self.aaudio.result_text(n));
                        self.report(true, libc::EIO);
                        std::thread::sleep(Duration::from_millis(50));
                        break;
                    }
                    if n == 0 {
                        // The server took nothing for 200 ms. Three in a row (600 ms) on a stream that is not the mixer
                        // track: its DSP never started (no timestamp; this phone's HAL does it on some MMAP exclusive
                        // opens) and no write will go in; AAudio holds nothing, so the read alone cannot tell. The
                        // receive loop reopens the speaker on the legacy path, once.
                        idle += 1;
                        if idle >= 3 {
                            let mut r = self.ring();
                            if r.out_path != OutPath::Legacy {
                                r.dead = true;
                                drop(r);
                                eprintln!(
                                    "oneios-audio: speaker: the server took nothing in 600 ms: the speaker is reopened \
                                     as a mixer track"
                                );
                                break;
                            }
                        }
                        continue;
                    }
                    idle = 0;
                    done += n as u32;
                    d.frames.fetch_add(n as u64, Ordering::Relaxed);
                }
            }
            if time::monotonic_s() - reported >= 10.0 {
                reported = time::monotonic_s();
                self.out_report("running");
                demote_realtime();
            }
            if self.ring().dead {
                break; // the receive loop reopens the speaker (restart_speaker)
            }
        }
    }

    fn reader(self: Arc<Helper>, d: Arc<Dir>) {
        let channels = d.channels.clamp(1, 8) as usize;
        let mut frames = vec![0i16; IN_PERIOD * 8];
        let mut reported = d.started();
        while d.run.load(Ordering::Acquire) {
            let n = d.stream.read(&mut frames, IN_PERIOD as i32, 200_000_000);
            if n < 0 {
                eprintln!("oneios-audio: microphone read: {}", self.aaudio.result_text(n));
                self.report(false, libc::EIO);
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            if n == 0 {
                continue;
            }
            if (n as usize) < IN_PERIOD {
                self.in_short.fetch_add(1, Ordering::Relaxed);
            }
            d.frames.fetch_add(n as u64, Ordering::Relaxed);
            let header = AuHeader { kind: AU_PCM, output: 0, channels: 1, value: n as u32, ..Default::default() };
            let first: Vec<i16> = (0..n as usize).map(|i| frames[i * channels]).collect(); // the first channel
            if self.send(&super::proto::au_message(header, &first)) < 0 {
                self.in_lost.fetch_add(1, Ordering::Relaxed);
            } else {
                self.in_sent.fetch_add(1, Ordering::Relaxed);
            }
            if time::monotonic_s() - reported >= 10.0 {
                reported = time::monotonic_s();
                eprintln!(
                    "oneios-audio: microphone running: {:.1} s, {} frames, {} periods sent, {} lost (xnuhost behind), {} \
                     short, {} xruns",
                    time::monotonic_s() - d.started(),
                    d.frames.load(Ordering::Relaxed),
                    self.in_sent.load(Ordering::Relaxed),
                    self.in_lost.load(Ordering::Relaxed),
                    self.in_short.load(Ordering::Relaxed),
                    d.stream.xrun_count()
                );
                demote_realtime();
            }
        }
    }

    fn stop_dir(&self, output: bool) {
        let Some(d) = self.dir(output) else { return };
        if output {
            self.ring().guest_on = false;
        }
        d.run.store(false, Ordering::Release);
        self.ring_more.notify_all();
        if let Some(t) = locked(if output { &self.out_thread } else { &self.in_thread }).take() {
            let _ = t.join();
        }
        if output {
            self.out_report("stopped");
        } else {
            eprintln!(
                "oneios-audio: microphone stopped: {:.1} s, {} frames, {} periods sent, {} lost, {} short",
                time::monotonic_s() - d.started(),
                d.frames.load(Ordering::Relaxed),
                self.in_sent.load(Ordering::Relaxed),
                self.in_lost.load(Ordering::Relaxed),
                self.in_short.load(Ordering::Relaxed)
            );
        }
        d.stream.request_stop();
        d.stream.close();
        *locked(if output { &self.out } else { &self.inp }) = None;
    }

    /// The guest's STOP of the speaker: the stream plays silence and stays open for linger_s.
    fn pause_output(&self) {
        if self.dir(true).is_none() || !self.ring().guest_on {
            return;
        }
        self.out_report("stopped by the guest");
        self.ring().guest_on = false; // the writer plays what the ring holds, then silence
        *locked(&self.idle_since) = time::monotonic_s();
    }

    /// A stream on (the guest's START); 0 or an errno.
    fn start_dir(self: &Arc<Helper>, output: bool, rate: u32, channels: u32) -> i32 {
        self.start_dir_with(output, rate, channels, false)
    }

    /// start_dir; `prefill`: the speaker's stream is filled with silence before its start (the probe's, probe_prefill).
    fn start_dir_with(self: &Arc<Helper>, output: bool, rate: u32, channels: u32, prefill: bool) -> i32 {
        if output {
            if let Some(d) = self.dir(true) {
                if rate == d.stream.sample_rate() as u32 && channels == d.channels {
                    // the lingering stream: on at once (a tail still in the ring plays first)
                    self.ring().restart();
                    *locked(&d.started) = time::monotonic_s();
                    return 0;
                }
            }
        }
        self.stop_dir(output);
        if output {
            let mut r = self.ring();
            r.consuming = false;
            r.allowance = RING_FRAMES;
            r.head = 0;
            r.count = 0;
            r.priming = true;
            r.restart();
        } else {
            self.in_sent.store(0, Ordering::Relaxed);
            self.in_lost.store(0, Ordering::Relaxed);
            self.in_short.store(0, Ordering::Relaxed);
        }
        let Some(stream) = self.open_stream(output, rate, channels, prefill) else { return libc::EIO };
        if output {
            self.host_volume_full(); // One UI's media volume to full scale, once a run
                                     // the path's floor (open_stream). On MMAP a new stream starts 5 ms above its floor: it comes down once a
                                     // quiet while shows the guest's periods on time (the writer's decay), not on the guest's first sound.
            let mut r = self.ring();
            let up = if r.continuous && self.settings.target_env_ms == 0 { 5 } else { 0 };
            r.target = ms_frames(r.target_ms + up);
            r.window_late_max = 0.0;
            r.window_periods = 0;
        }
        // what AAudio already holds (0 but for a prefilled probe stream, whose silence its timestamps count: without it
        // pipeline_frames would come out short by the buffer)
        let written = if output { stream.frames_written().max(0) as u64 } else { 0 };
        let d = Arc::new(Dir {
            burst: usable_burst(stream.frames_per_burst()),
            channels: stream.channel_count() as u32,
            stream,
            run: AtomicBool::new(true),
            frames: AtomicU64::new(written),
            started: Mutex::new(time::monotonic_s()),
        });
        *locked(if output { &self.out } else { &self.inp }) = Some(Arc::clone(&d));
        let (me, dir) = (Arc::clone(self), Arc::clone(&d));
        let spawned = if output {
            xh_core::thread::spawn("audio-writer", move || me.writer(dir))
        } else {
            xh_core::thread::spawn("audio-reader", move || me.reader(dir))
        };
        match spawned {
            Ok(t) => *locked(if output { &self.out_thread } else { &self.in_thread }) = Some(t),
            Err(_) => {
                self.stop_dir(output);
                return libc::EAGAIN;
            }
        }
        demote_realtime();
        0
    }

    fn put_pcm(&self, pcm: &[i16], frames: u32) {
        // what AAudio holds that is not yet presented (None: no timestamp)
        let pipeline = self.dir(true).and_then(|d| d.pipeline_frames());
        let mut r = self.ring();
        r.stats.r#in += u64::from(frames);
        if !r.guest_on {
            return;
        }
        let ahead = pipeline.map(|p| p + i64::from(r.count));
        r.guest_period(frames, ahead);
        // Before the server takes its first frames (a new track's stall, about 200 ms) the whole ring holds: the start
        // of a short sound is all there is of it.
        let limit =
            if !r.consuming { RING_FRAMES } else { (r.allowance + ms_frames(40)).max(r.target + ms_frames(60)) }
                .min(RING_FRAMES);
        if r.count + frames > limit {
            // far ahead of the DAC: drop the oldest audio down to the target
            let mut keep = r.target.min(r.count);
            if keep + frames > RING_FRAMES {
                keep = RING_FRAMES - frames;
            }
            r.head = (r.head + r.count - keep) % RING_FRAMES;
            r.stats.overflows += 1;
            r.generation += 1; // the level after a stall is no reference for the drift
            r.stats.dropped += u64::from(r.count - keep);
            r.count = keep;
        }
        for i in 0..frames {
            let at = 2 * ((r.head + r.count + i) % RING_FRAMES) as usize;
            r.ring[at] = pcm[2 * i as usize];
            r.ring[at + 1] = pcm[2 * i as usize + 1];
        }
        r.count += frames;
        if r.count > r.stats.fill_max {
            r.stats.fill_max = r.count;
        }
        drop(r);
        self.ring_more.notify_one();
    }

    /// The writer saw the server take no frames for 1 s (its DSP never started, or stopped): the speaker closed and
    /// opened again on the legacy path (mmap_dead), the guest's stream kept (guest_on, which stop_dir clears: put back
    /// after). 0 or start_dir's errno.
    fn restart_speaker(self: &Arc<Helper>) -> i32 {
        let Some(d) = self.dir(true) else { return 0 };
        let (rate, channels) = (d.stream.sample_rate().max(0) as u32, d.channels);
        let guest_on = self.ring().guest_on;
        self.mmap_dead.store(true, Ordering::Relaxed);
        self.stop_dir(true);
        let rc = self.start_dir(true, rate, channels);
        if rc != 0 {
            return rc;
        }
        self.ring().guest_on = guest_on; // the guest's stream is up: its next period plays on the new track
        if !guest_on {
            *locked(&self.idle_since) = time::monotonic_s();
        }
        0
    }

    /// AU_PROBE (xnuhost, before the guest boots): the speaker's stream opened now and kept in silence (it lingers, so
    /// iOS's first sound finds it open), and its latency for a guest period: what AAudio holds below its read (the
    /// median of 10 looks once the server reads and gives a timestamp, up to 1.5 s for that: a mixer track's start
    /// stall is about 200 ms) plus the level the writer holds when a period comes in (the target; on a mixer also half
    /// a pull). Frames (0: not measured).
    fn probe(self: &Arc<Helper>) -> u32 {
        // Two tries: the stream on the configured path, then (when its server took no frame) one on the legacy path.
        // The answer must come well within xnuhost's 2 s (session/bridges.rs): a dead stream is given up as soon as
        // the writer says so (its 600 ms of timed-out writes or a second with AAudio's frames unread), and the legacy
        // path is not waited on at all (its silence never reaches AudioFlinger's start threshold without the guest).
        for attempt in 0..2 {
            if self.dir(true).is_none() {
                if self.start_dir_with(true, RATE, 2, self.settings.probe_prefill) != 0 {
                    eprintln!("oneios-audio: probe: the speaker did not open");
                    return 0;
                }
                self.ring().guest_on = false; // silence until the guest's START, then the linger
                *locked(&self.idle_since) = time::monotonic_s();
            }
            let Some(d) = self.dir(true) else { return 0 };
            if attempt != 0 {
                eprintln!(
                    "oneios-audio: probe: not measured on the mixer track (it starts on the guest's first period)"
                );
                return 0;
            }
            let mut samples = Vec::with_capacity(10);
            for _ in 0..150 {
                if samples.len() >= 10 || self.ring().dead {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
                if d.stream.frames_read() > 0 {
                    if let Some(below) = d.below_read() {
                        samples.push(below);
                    }
                }
            }
            if samples.is_empty() || self.ring().dead {
                let path = self.ring().out_path;
                if path != OutPath::Legacy {
                    // The server never took a frame: the MMAP stream's DSP did not start (this phone's HAL does it
                    // now and then). The writer would then wait 200 ms per write forever and drop every guest period:
                    // the legacy mixer track instead.
                    self.mmap_dead.store(true, Ordering::Relaxed);
                    eprintln!(
                        "oneios-audio: probe: the server took no frames on {}: reopened as a mixer track",
                        path_name(path)
                    );
                    self.stop_dir(true);
                    continue;
                }
                eprintln!(
                    "oneios-audio: probe: no timestamp from the speaker's stream in 1.5 s: not measured (a mixer \
                     track starts on the guest's first period)"
                );
                return 0;
            }
            samples.sort_unstable();
            let median = samples[samples.len() / 2] as u32;
            let (level, path) = {
                let r = self.ring();
                let extra = match r.out_path {
                    OutPath::MmapShared => d.burst,
                    _ if r.continuous => 0,
                    _ => (if r.pulls.size != 0 { r.pulls.size } else { d.burst }) / 2,
                };
                (r.target + extra, r.out_path)
            };
            let frames = median + level;
            eprintln!(
                "oneios-audio: probe: speaker latency {frames} frames ({} ms) on {}: {} ms below AAudio's read + {} ms \
                 of level when a period comes in",
                frames_ms(frames),
                path_name(path),
                frames_ms(median),
                frames_ms(level)
            );
            return frames;
        }
        0
    }

    /// The receive loop until xnuhost closes its end: 0.
    pub fn run(self: &Arc<Helper>) -> i32 {
        let s = &self.settings;
        let buffer = if s.buffer_bursts != 0 {
            format!("{} bursts", s.buffer_bursts)
        } else {
            "a burst + the target + 5 ms".to_owned()
        };
        let target_text = if s.target_env_ms != 0 {
            format!("{} ms", s.target_env_ms)
        } else {
            "5 ms on MMAP (a new stream at 10), 20 ms on a mixer".to_owned()
        };
        let mmap = if s.perf_mode != PERF_LOW_LATENCY || s.mmap_want == MmapWant::Off {
            "off"
        } else if s.mmap_want == MmapWant::Exclusive {
            "exclusive first (shared only with system_server)"
        } else {
            "shared (only with system_server)"
        };
        eprintln!(
            "oneios-audio: ready: performance mode {}, MMAP {mmap} (a blocking writer at SCHED_OTHER, uclamp.min {}: no \
             data callback, no realtime thread), AAudio buffer {buffer}, ring {} ms, target {target_text} (at most 80, \
             down 5 ms per {} s without an underrun), linger {} s",
            perf_name(s.perf_mode),
            s.uclamp_min,
            frames_ms(RING_FRAMES),
            s.target_decay_s,
            s.linger_s
        );
        let mut packet = vec![0u8; AU_HEADER + AU_MAX_FRAMES as usize * 4];
        loop {
            if self.ring().dead {
                let rc = self.restart_speaker();
                if rc != 0 {
                    self.report(true, rc);
                }
                continue;
            }
            let mut wait = libc::pollfd { fd: self.sock.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: one valid pollfd.
            if unsafe { libc::poll(&mut wait, 1, 1000) } == 0 {
                // a quiet second: close a stream that lingered long enough
                let lingered = time::monotonic_s() - *locked(&self.idle_since) >= f64::from(s.linger_s);
                if self.dir(true).is_some() && !self.ring().guest_on && lingered {
                    self.stop_dir(true);
                    eprintln!("oneios-audio: speaker closed after {} s of silence", s.linger_s);
                }
                continue;
            }
            // SAFETY: a valid buffer of its length.
            let got = unsafe {
                libc::recv(self.sock.as_raw_fd(), packet.as_mut_ptr().cast(), packet.len(), libc::MSG_DONTWAIT)
            };
            if got < 0 {
                match errno() {
                    libc::EAGAIN | libc::EINTR => continue,
                    _ => break,
                }
            }
            if got == 0 {
                break; // xnuhost closed its end (or exited)
            }
            let got = got as usize;
            if got < AU_HEADER {
                continue;
            }
            let h = AuHeader::read_from(&packet);
            let output = h.output != 0;
            match h.kind {
                AU_START => {
                    let rc = self.start_dir(output, h.value, h.channels.into());
                    if rc != 0 {
                        self.report(output, rc);
                    }
                }
                AU_STOP => {
                    if output && s.linger_s != 0 {
                        self.pause_output();
                    } else {
                        self.stop_dir(output);
                    }
                }
                AU_PCM => {
                    let frames = (((got - AU_HEADER) / 4) as u32).min(h.value);
                    if output && self.dir(true).is_some() && frames != 0 {
                        let pcm: Vec<i16> =
                            super::proto::samples(&packet[AU_HEADER..AU_HEADER + frames as usize * 4]).collect();
                        self.put_pcm(&pcm, frames);
                    }
                }
                AU_PROBE => {
                    let frames = self.probe();
                    let reply = AuHeader { kind: AU_LATENCY, output: 1, value: frames, ..Default::default() };
                    let _ = self.send(&reply.to_bytes());
                }
                AU_TOLD => {
                    self.told_frames.store(h.value, Ordering::Relaxed);
                    eprintln!(
                        "oneios-audio: iOS is told an output latency of {} frames ({} ms)",
                        h.value,
                        frames_ms(h.value)
                    );
                }
                _ => {}
            }
        }
        self.stop_dir(true);
        self.stop_dir(false);
        self.host_volume_restore();
        eprintln!("oneios-audio: socket closed: exit");
        0
    }
}

/// The program: `oneios-audio SOCKET_FD` (started by xnuhost) on `aaudio`. Its exit code.
pub fn main_with(args: &[String], aaudio: Arc<dyn AAudio>) -> i32 {
    if args.len() != 2 {
        eprintln!("usage: oneios-audio SOCKET_FD (started by xnuhost)");
        return 2;
    }
    let fd: i32 = xh_core::parse::leading_i64(&args[1], 10).map_or(0, |(v, _)| v as i32);
    // SAFETY: the descriptor xnuhost handed this process (its end of the pair), owned from here.
    let sock = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    Helper::new(sock, aaudio).run()
}

#[cfg(test)]
mod raise_tests {
    use super::*;

    /// A burst of xrun/underrun reports inside one guest period raises the target once; the next period's raises
    /// again; a new stream (restart) starts uncoalesced. The ceiling still holds.
    #[test]
    fn raise_target_coalesces_per_guest_period() {
        let mut r = Ring::new();
        r.target_ms = 5;
        r.target_max_ms = 80;
        r.target = ms_frames(5);
        let t0 = 100.0;
        assert!(r.raise_target(t0));
        // 18 reports in 0.3 ms (the SM8850's MMAP counter): one starvation, one raise
        for i in 1..18 {
            assert!(!r.raise_target(t0 + f64::from(i) * 0.3e-3 / 18.0));
        }
        assert_eq!(r.target, ms_frames(10));
        assert_eq!(r.stats.target_peak, ms_frames(10));
        // a period later: a new episode
        // just past a period (exactly 5 ms after t0 rounds to under it)
        assert!(r.raise_target(t0 + f64::from(CHUNK) / f64::from(RATE) + 1e-6));
        assert_eq!(r.target, ms_frames(15));
        r.restart();
        assert!(r.raise_target(t0 + 0.006));
        for k in 0..40 {
            r.raise_target(t0 + 1.0 + f64::from(k) * 0.01);
        }
        assert_eq!(r.target, ms_frames(80)); // the ceiling
    }
}

#[cfg(test)]
mod prefill_tests {
    use super::*;

    /// A stream whose buffer takes `room` frames before its start, then refuses (a non-blocking write: 0).
    struct Room {
        size: i32,
        capacity: i32,
        room: i32,
        written: Mutex<i64>,
        timeouts: Mutex<Vec<i64>>,
    }

    impl AStream for Room {
        fn request_start(&self) -> i32 {
            0
        }
        fn request_stop(&self) -> i32 {
            0
        }
        fn close(&self) {}
        fn write(&self, pcm: &[i16], frames: i32, timeout_ns: i64) -> i32 {
            assert!(pcm.len() >= frames as usize * 2 && pcm.iter().all(|&v| v == 0));
            locked(&self.timeouts).push(timeout_ns);
            let mut w = locked(&self.written);
            let n = (i64::from(self.room) - *w).clamp(0, i64::from(frames)) as i32;
            *w += i64::from(n);
            n
        }
        fn read(&self, _: &mut [i16], _: i32, _: i64) -> i32 {
            0
        }
        fn set_buffer_size(&self, f: i32) -> i32 {
            f
        }
        fn timestamp(&self) -> Option<(i64, i64)> {
            None
        }
        fn frames_per_burst(&self) -> i32 {
            192
        }
        fn buffer_size(&self) -> i32 {
            self.size
        }
        fn buffer_capacity(&self) -> i32 {
            self.capacity
        }
        fn sample_rate(&self) -> i32 {
            RATE as i32
        }
        fn channel_count(&self) -> i32 {
            2
        }
        fn device_id(&self) -> i32 {
            0
        }
        fn session_id(&self) -> i32 {
            0
        }
        fn xrun_count(&self) -> i32 {
            0
        }
        fn sharing_mode(&self) -> i32 {
            0
        }
        fn performance_mode(&self) -> i32 {
            0
        }
        fn frames_written(&self) -> i64 {
            *locked(&self.written)
        }
        fn frames_read(&self) -> i64 {
            0
        }
        fn mmap_used(&self) -> i32 {
            1
        }
    }

    fn room(size: i32, capacity: i32, room: i32) -> Room {
        Room { size, capacity, room, written: Mutex::new(0), timeouts: Mutex::new(Vec::new()) }
    }

    #[test]
    fn prefill_fills_the_buffer_without_blocking() {
        // the SM8850's MMAP shape: 576 frames of 3072, in chunks, every write non-blocking
        let s = room(576, 3072, 3072);
        assert!(prefill_speaker(&s));
        assert_eq!(s.frames_written(), 576);
        assert!(locked(&s.timeouts).iter().all(|&t| t == 0) && locked(&s.timeouts).len() == 3);
        // a short write (the buffer full early): not started
        assert!(!prefill_speaker(&room(576, 3072, 480)));
        // an unusable buffer: nothing written
        let bad = room(0, 3072, 3072);
        assert!(!prefill_speaker(&bad) && bad.frames_written() == 0);
        assert!(!prefill_speaker(&room(4096, 3072, 9999)));
        assert!(!prefill_speaker(&room(RING_FRAMES as i32 + 1, 99999, 99999)));
    }

    #[test]
    fn probe_prefill_is_off_unless_asked() {
        let _g = crate::hw::testdir::env_lock();
        std::env::remove_var("ONEIOS_AUDIO_PROBE_PREFILL");
        assert!(!Settings::from_env().probe_prefill);
        std::env::set_var("ONEIOS_AUDIO_PROBE_PREFILL", "1");
        assert!(Settings::from_env().probe_prefill);
        std::env::set_var("ONEIOS_AUDIO_PROBE_PREFILL", "0");
        assert!(!Settings::from_env().probe_prefill);
        std::env::remove_var("ONEIOS_AUDIO_PROBE_PREFILL");
    }
}
