// SPDX-License-Identifier: MIT
//! Host test of the AAudio helper (C: tests/test_audio_helper.c and its fake AAudio, tests/fake/aaudio) against a fake
//! AAudio. The fake speaker is a stand-in for AudioFlinger's normal mixer: a server thread that, once the track has
//! been filled to its buffer size (AudioFlinger's start threshold) and after a start stall, takes `pull` frames every
//! pull period on its own clock, and takes nothing in a period where the track holds less (AudioFlinger mixes a normal
//! track only when it holds a whole period). The phone's shape is burst 1600, pull 1600 (33 ms), buffer 3200
//! (docs/hardware.md section 6). The fake microphone gives 48 kHz in real time. A producer plays the guest: 240-frame
//! periods every 5 ms on a clock that runs faster or slower than the server's.
//!
//! The fake HAL may also have MMAP: a LOW_LATENCY stream is then MMAP exclusive or shared, read a burst at a time from
//! its start (the DSP, AAudio's endpoint), its timestamp a fixed number of frames behind its read; the helper's path
//! choice (exclusive, shared only with system_server, a mixer track), its level on MMAP, its end-to-end figure and the
//! probe are checked against it.
//! Checks: no underrun of the helper or of the server after the start, no overflow, the drift steered the right way,
//! the latency bounded, the microphone's periods reach the socket, the stop is clean, the linger, the target back to its
//! floor after underruns, a stall's backlog gone at the first silence.
//! Latency per stage (printed as "stage: ..."): marker periods (240 equal frames, a value no other frame has) are timed
//! when the guest sends them, when the helper writes them to AAudio (socket + ring) and when the server reads them
//! (AAudio's queue); Android's mixer and HAL after that are not modelled (the phone's share, docs/hardware.md).
//!
//! It runs about a minute of real time, as the C test did.

use super::aaudio_helper::*;
use super::proto::{
    au_message, AuHeader, AU_HEADER, AU_LATENCY, AU_MAX_FRAMES, AU_PCM, AU_PROBE, AU_START, AU_STOP, AU_TOLD,
};
use crate::hw::testdir::env_lock;
use crate::proto::Wire;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use xh_core::time;

const MARKS: usize = 256;
/// An idle host runs the test's threads within about 1 ms.
const HOST_LATE: f64 = 0.004;

/// Sleeps until CLOCK_MONOTONIC reads `at` (s).
fn sleep_until(at: f64) {
    let t = libc::timespec { tv_sec: at as libc::time_t, tv_nsec: ((at - at.floor()) * 1e9) as libc::c_long };
    // SAFETY: a valid timespec; an absolute sleep on CLOCK_MONOTONIC.
    unsafe { libc::clock_nanosleep(libc::CLOCK_MONOTONIC, libc::TIMER_ABSTIME, &t, std::ptr::null_mut()) };
}

fn nap_s(s: f64) {
    std::thread::sleep(std::time::Duration::from_secs_f64(s));
}

fn mark_value(k: usize) -> i16 {
    (3001 + 7 * k) as i16
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hal {
    NoMmap,
    MmapShared,
    MmapExclusive,
}

/// The fake's knobs and what it saw (C: its statics).
struct Knobs {
    /// the server's rate against the producer's clock
    dac_ppm: f64,
    /// a new track takes nothing for this long after it is filled
    dac_stall_s: f64,
    dac_pull: i32,
    dac_burst: i32,
    /// what the fake AAudio_setMMapPolicy answers
    mmap_policy_rc: i32,
    mmap_policy_set: i32,
    /// a mixer track that says MMAP anyway
    fake_mmap_used: bool,
    /// The fake HAL's MMAP: none (a LOW_LATENCY open gets a mixer track, as AAudio's fallback), shared only, or
    /// exclusive too. An MMAP stream's reader takes `mmap_burst` frames every burst period from its start on, whatever
    /// the stream holds, and its timestamp presents a frame `mmap_below` frames after its read (the HAL and the DSP).
    hal: Hal,
    mmap_burst: i32,
    mmap_capacity: i32,
    mmap_below: i64,
    /// An MMAP stream whose reader never takes a frame (the DSP did not start: the S25's HAL now and then); set while
    /// a run is going and that stream's server stops taking too (what the S25's dead stream did: nothing).
    dead_mmap: bool,
    /// The audio policy's media index on the speaker (fix-audio-volume).
    media_index: i32,
    media_sets: u32,
    media_bad: u32,
    last_out: Option<Arc<FakeStream>>,
    last_in: Option<Arc<FakeStream>>,
    out_opens: u32,
}

struct Fake {
    knobs: Mutex<Knobs>,
    /// How late the fake server's pulls and the guest's sends ran against their schedules (s), as f64 bits.
    host_late_max: AtomicU64,
}

impl Fake {
    fn k(&self) -> MutexGuard<'_, Knobs> {
        self.knobs.lock().unwrap()
    }
    fn late(&self, late: f64) {
        let mut cur = self.host_late_max.load(Ordering::Relaxed);
        while late > f64::from_bits(cur) {
            match self.host_late_max.compare_exchange(cur, late.to_bits(), Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(c) => cur = c,
            }
        }
    }
    fn late_max(&self) -> f64 {
        f64::from_bits(self.host_late_max.load(Ordering::Relaxed))
    }
    fn reset_late(&self) {
        self.host_late_max.store(0f64.to_bits(), Ordering::Relaxed);
    }
    /// The checks that need an on-time host are skipped (and say so) when the host ran this test's threads late.
    fn host_on_time(&self, what: &str) -> bool {
        if self.late_max() <= HOST_LATE {
            return true;
        }
        eprintln!("   the host ran this test's threads up to {:.1} ms late: {what} not checked", self.late_max() * 1e3);
        false
    }
    fn last_out(&self) -> Arc<FakeStream> {
        self.k().last_out.clone().unwrap()
    }
}

#[derive(Default)]
struct StreamState {
    buffer: i32,
    /// the microphone's start; the speaker's first write
    t0: f64,
    written: i64,
    read: i64,
    dac_underflows: u64,
    nonzero: u64,
    pulls_done: u64,
    /// when the server last took frames
    last_pull: f64,
    // markers: the guest's send time (filled by the test), the write time and position, the read time
    next_mark: usize,
    sent: Vec<f64>,
    wrote: Vec<f64>,
    taken: Vec<f64>,
    at: Vec<i64>,
}

struct FakeStream {
    fake: Arc<Fake>,
    rate: i32,
    channels: i32,
    burst: i32,
    pull: i32,
    perf: i32,
    sharing: i32,
    capacity: i32,
    /// a mixer track (false) or MMAP (exclusive: the DSP, shared: AAudio's endpoint)
    mmap: bool,
    s: Mutex<StreamState>,
    quit: AtomicBool,
    closed: AtomicBool,
    server: Mutex<Option<JoinHandle<()>>>,
}

impl FakeStream {
    fn st(&self) -> MutexGuard<'_, StreamState> {
        self.s.lock().unwrap()
    }

    /// The server: once filled to the buffer size and after the stall, a pull every pull period on its own clock. An
    /// MMAP stream's reader instead runs from the start: a burst every burst period.
    fn server(self: Arc<FakeStream>) {
        let (ppm, stall) = {
            let k = self.fake.k();
            (k.dac_ppm, k.dac_stall_s)
        };
        let period = f64::from(self.pull) / f64::from(self.rate) / (1.0 + ppm / 1e6);
        if !self.mmap {
            // AudioFlinger's start threshold: the buffer filled once
            loop {
                if self.quit.load(Ordering::Relaxed) {
                    return;
                }
                let full = {
                    let s = self.st();
                    s.written - s.read >= i64::from(s.buffer)
                };
                if full {
                    break;
                }
                nap_s(0.0002);
            }

            nap_s(stall);
        }
        let start = time::monotonic_s();
        let mut n = 1u64;
        while !self.quit.load(Ordering::Relaxed) {
            let due = start + n as f64 * period;
            n += 1;
            sleep_until(due);
            let mut s = self.st();
            let now = time::monotonic_s();
            self.fake.late(now - due);
            let pull = i64::from(self.pull);
            let took = if self.mmap && self.fake.k().dead_mmap {
                false // the DSP never started (or stopped): the stream takes nothing
            } else if self.mmap {
                // A short buffer is an xrun (the DSP plays what is there, then stale data or silence). Counted in the
                // client's frames, as AAudio's catch-up keeps them: the read never passes the write.
                let have = s.written - s.read;
                if have < pull && s.written > 0 {
                    s.dac_underflows += 1; // before the first write it is the stream's start
                }
                s.read += have.min(pull);
                true
            } else if s.written - s.read >= pull {
                s.read += pull;
                true
            } else {
                if s.pulls_done != 0 {
                    s.dac_underflows += 1; // not ready: nothing taken this period
                }
                false
            };
            if took {
                s.pulls_done += 1;
                s.last_pull = now;
                for k in 0..s.next_mark {
                    if s.taken[k] == 0.0 && s.at[k] >= 0 && s.at[k] < s.read {
                        s.taken[k] = now;
                    }
                }
            }
        }
    }
}

impl AStream for FakeStream {
    fn request_start(&self) -> i32 {
        self.st().t0 = time::monotonic_s();
        0
    }
    fn request_stop(&self) -> i32 {
        0
    }
    fn close(&self) {
        // the stream is kept for the checks; its server stops
        self.quit.store(true, Ordering::Relaxed);
        if let Some(t) = self.server.lock().unwrap().take() {
            let _ = t.join();
        }
        self.closed.store(true, Ordering::Relaxed);
    }
    fn write(&self, pcm: &[i16], frames: i32, timeout_ns: i64) -> i32 {
        // As AudioTrack's blocking write: what fits goes at once, the rest as room comes, until the timeout.
        let t0 = time::monotonic_s();
        let mut done = 0i32;
        let channels = self.channels as usize;
        while done < frames {
            let mut s = self.st();
            let room = i64::from(s.buffer) - (s.written - s.read);
            if room <= 0 {
                drop(s);
                if (time::monotonic_s() - t0) * 1e9 > timeout_ns as f64 {
                    return done;
                }
                nap_s(0.0002);
                continue;
            }
            let n = (frames - done).min(room as i32);
            let now = time::monotonic_s();
            let (from, to) = (done as usize * channels, (done + n) as usize * channels);
            for (i, &v) in pcm.iter().enumerate().take(to).skip(from) {
                if v != 0 {
                    s.nonzero += 1;
                }
                let k = s.next_mark;
                if channels == 2 && i % 2 == 0 && k < MARKS && s.sent[k] > 0.0 && v == mark_value(k) {
                    s.wrote[k] = now;
                    s.at[k] = s.written + (i / 2) as i64 - i64::from(done);
                    s.next_mark += 1;
                }
            }
            s.written += i64::from(n);
            done += n;
        }
        frames
    }
    fn read(&self, pcm: &mut [i16], frames: i32, _timeout_ns: i64) -> i32 {
        loop {
            let (t0, read) = {
                let s = self.st();
                (s.t0, s.read)
            };
            if ((time::monotonic_s() - t0) * f64::from(self.rate)) as i64 >= read + i64::from(frames) {
                break;
            }
            nap_s(0.0005);
        }
        let mut s = self.st();
        let channels = self.channels as usize;
        for (i, v) in pcm.iter_mut().take(frames as usize * channels).enumerate() {
            *v = (100 + (s.read + (i / channels) as i64) % 1000) as i16;
        }
        s.read += i64::from(frames);
        frames
    }
    fn set_buffer_size(&self, frames: i32) -> i32 {
        let mut s = self.st();
        let frames = frames.max(self.burst).min(self.capacity); // AAudio: never less than a burst
        if frames > 0 {
            s.buffer = frames;
        }
        s.buffer
    }
    fn timestamp(&self) -> Option<(i64, i64)> {
        let s = self.st();
        let below = self.fake.k().mmap_below;
        let ok = s.pulls_done > 0 && !self.fake.k().dead_mmap && (!self.mmap || s.read >= below);
        // MMAP: the HAL and the DSP after the read
        ok.then(|| (if self.mmap { s.read - below } else { s.read }, (s.last_pull * 1e9) as i64))
    }
    fn frames_per_burst(&self) -> i32 {
        self.burst
    }
    fn buffer_size(&self) -> i32 {
        self.st().buffer
    }
    fn buffer_capacity(&self) -> i32 {
        self.capacity
    }
    fn sample_rate(&self) -> i32 {
        self.rate
    }
    fn channel_count(&self) -> i32 {
        self.channels
    }
    fn device_id(&self) -> i32 {
        3
    }
    fn session_id(&self) -> i32 {
        -1
    }
    fn xrun_count(&self) -> i32 {
        self.st().dac_underflows as i32
    }
    fn sharing_mode(&self) -> i32 {
        self.sharing
    }
    /// libaudioclient takes FAST off a blocking writer's track: what is given is NONE. An MMAP stream is LOW_LATENCY.
    fn performance_mode(&self) -> i32 {
        if self.mmap {
            PERF_LOW_LATENCY
        } else {
            PERF_NONE
        }
    }
    fn frames_written(&self) -> i64 {
        self.st().written
    }
    /// A mixer track's read moves a pull at a time; an MMAP stream's is AAudio's clock model, which moves on between
    /// the DSP's bursts (never past what was written).
    fn frames_read(&self) -> i64 {
        let s = self.st();
        let mut v = s.read;
        if self.mmap && s.pulls_done != 0 && !self.fake.k().dead_mmap {
            let on = ((time::monotonic_s() - s.last_pull) * f64::from(self.rate)) as i64;
            v += on.clamp(0, i64::from(self.pull));
            if v > s.written {
                v = s.written.max(s.read);
            }
        }
        v
    }
    fn mmap_used(&self) -> i32 {
        i32::from(self.mmap || self.fake.k().fake_mmap_used)
    }
}

struct FakeAAudio(Arc<Fake>);

impl AAudio for FakeAAudio {
    fn open(&self, r: &StreamRequest) -> Result<Arc<dyn AStream>, i32> {
        let mut k = self.0.k();
        let (mut burst, mut pull) = if r.output { (k.dac_burst, k.dac_pull) } else { (960, 960) };
        let mut sharing = SHARING_SHARED;
        // AAudio's MMAP: only for LOW_LATENCY, only when the policy is not NEVER (AudioStreamBuilder::build), exclusive
        // only when asked and the HAL has it (else shared).
        let mmap = r.output && r.perf == PERF_LOW_LATENCY && k.mmap_policy_set != POLICY_NEVER && k.hal != Hal::NoMmap;
        if mmap {
            burst = k.mmap_burst;
            pull = k.mmap_burst;
            if r.sharing == SHARING_EXCLUSIVE && k.hal == Hal::MmapExclusive {
                sharing = SHARING_EXCLUSIVE;
            }
        }
        let capacity = if mmap { k.mmap_capacity } else { 3 * burst };
        let buffer = if mmap { 2 * burst } else { 3 * burst };
        let s = Arc::new(FakeStream {
            fake: Arc::clone(&self.0),

            rate: r.rate as i32,
            channels: r.channels as i32,
            burst,
            pull,
            perf: r.perf,
            sharing,
            capacity,
            mmap,
            s: Mutex::new(StreamState {
                buffer,
                sent: vec![0.0; MARKS],
                wrote: vec![0.0; MARKS],
                taken: vec![0.0; MARKS],
                at: vec![-1; MARKS],
                ..StreamState::default()
            }),
            quit: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            server: Mutex::new(None),
        });
        if r.output {
            k.last_out = Some(Arc::clone(&s));
            k.out_opens += 1;
            let server = Arc::clone(&s);
            *s.server.lock().unwrap() = Some(std::thread::spawn(move || server.server()));
        } else {
            k.last_in = Some(Arc::clone(&s));
        }
        Ok(s)
    }
    fn result_text(&self, rc: i32) -> String {
        if rc != 0 { "error" } else { "ok" }.to_owned()
    }
    fn set_mmap_policy(&self, policy: i32) -> Option<i32> {
        let mut k = self.0.k();
        if k.mmap_policy_rc == 0 {
            k.mmap_policy_set = policy;
        }
        Some(k.mmap_policy_rc)
    }
    fn volume_calls(&self) -> VolumeCalls {
        VolumeCalls { get: true, max: true, set: false, set_muted: true } // the Android 16 form of the set, as on the phone
    }
    fn volume_index(&self) -> (i32, i32) {
        (0, self.0.k().media_index)
    }
    fn volume_max(&self) -> (i32, i32) {
        (0, 15)
    }
    fn set_volume_index(&self, index: i32) -> i32 {
        let mut k = self.0.k();
        if !(0..=15).contains(&index) {
            k.media_bad += 1;
            return -22;
        }
        k.media_index = index;
        k.media_sets += 1;
        0
    }
}

// ---- the test -----------------------------------------------------------------------------------
/// The helper in a thread on one end of a socket pair; the test is the guest (xnuhost) on the other.
struct Run {
    guest: OwnedFd,
    helper: Arc<Helper>,
    thread: Option<JoinHandle<i32>>,
}

fn pair() -> (OwnedFd, OwnedFd) {
    xh_core::fd::socket_pair(libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC).unwrap()
}

fn send(fd: RawFd, bytes: &[u8]) {
    // SAFETY: a valid buffer of its length.
    let n = unsafe { libc::send(fd, bytes.as_ptr().cast(), bytes.len(), libc::MSG_NOSIGNAL) };
    assert_eq!(n, bytes.len() as isize);
}

impl Run {
    fn up(fake: &Arc<Fake>, linger: &str) -> Run {
        std::env::set_var("ONEIOS_AUDIO_LINGER_S", linger);
        let (guest, theirs) = pair();
        let helper = Helper::new(theirs, Arc::new(FakeAAudio(Arc::clone(fake))));
        let h = Arc::clone(&helper);
        let thread = Some(std::thread::spawn(move || h.run()));
        Run { guest, helper, thread }
    }

    fn down(self) {
        let Run { guest, helper, thread } = self;
        drop(guest); // xnuhost goes: the helper stops its streams and returns
        assert_eq!(thread.unwrap().join().unwrap(), 0);
        assert_eq!(helper.streams_open(), (false, false));
    }

    fn header(&self, kind: u8, output: bool, channels: u8, value: u32) {
        let h = AuHeader { kind, output: output.into(), channels, value, ..Default::default() };
        send(self.guest.as_raw_fd(), &h.to_bytes());
    }

    fn stats(&self) -> OutStats {
        self.helper.ring().stats
    }

    /// The helper's receive loop may still have periods in its socket on a loaded host: up to 1 s for them to come in.
    fn wait_in(&self, frames: u64) {
        for _ in 0..500 {
            if self.stats().r#in >= frames {
                return;
            }
            nap_s(0.002);
        }
    }
}

/// The guest: `seconds` of 240-frame periods, its clock `ppm` faster than the server's. A quiet sine, or digital
/// silence (iOS's engine running with nothing to play). Every `mark_every`-th period is a marker (0: none). Every
/// `late_every`-th period the guest's vCPU is late by `late` periods: they come back to back after it.
#[derive(Clone, Copy, Default)]
struct GuestPlay {
    seconds: f64,
    ppm: f64,
    silence: bool,
    mark_every: u32,
    late_every: u32,
    late: u32,
}

struct Player {
    fake: Arc<Fake>,
    marks_sent: usize,
}

impl Player {
    fn play_as(&mut self, run: &Run, g: GuestPlay) {
        let period = 5e-3 / (1.0 + g.ppm / 1e6);
        let start = time::monotonic_s();
        let mut phase = 0f64;
        let mut lost = 0u32; // periods of lateness so far (the schedule catches up)
        let mut pcm = vec![0i16; 480];
        for n in 0..(g.seconds * 200.0) as u32 {
            let mark = g.mark_every != 0 && n % g.mark_every == g.mark_every / 2 && self.marks_sent < MARKS;
            for i in 0..240 {
                let v = if mark {
                    mark_value(self.marks_sent)
                } else if g.silence {
                    0
                } else {
                    (1000.0 * phase.sin()) as i16
                };
                pcm[2 * i] = v;
                pcm[2 * i + 1] = v;
                phase += 2.0 * std::f64::consts::PI * 440.0 / 48000.0;
            }
            if mark {
                self.fake.last_out().st().sent[self.marks_sent] = time::monotonic_s();
                self.marks_sent += 1;
            }
            let h = AuHeader { kind: AU_PCM, output: 1, channels: 2, value: 240, ..Default::default() };
            send(run.guest.as_raw_fd(), &au_message(h, &pcm));
            // Late: nothing for `late` periods, then those come back to back (the schedule is absolute).
            let next = if g.late_every != 0 && n % g.late_every == g.late_every - 1 {
                lost = g.late;
                start + f64::from(n + 1 + lost) * period
            } else if lost != 0 {
                lost -= 1;
                continue;
            } else {
                start + f64::from(n + 1) * period
            };
            sleep_until(next);
            self.fake.late(time::monotonic_s() - next);
        }
    }

    fn play(&mut self, run: &Run, seconds: f64, ppm: f64) {
        self.play_as(run, GuestPlay { seconds, ppm, ..GuestPlay::default() });
    }
}

/// The stages of the markers sent from `first` on: socket + ring (sent to written), AAudio (written to read).
#[derive(Clone, Copy, Default, Debug)]
struct Stages {
    ring_ms: f64,
    aaudio_ms: f64,
    total_ms: f64,
    max_ms: f64,
    e2e_ms: f64,
    late_ms: f64,
    n: u32,
    e2e_n: u32,
}

fn measure(fake: &Fake, name: &str, first: usize) -> Stages {
    let s = fake.last_out();
    let st = s.st();
    let mut r = Stages::default();
    let (mut ring_max, mut aaudio_max) = (0f64, 0f64);
    for k in first..st.next_mark {
        if st.taken[k] == 0.0 {
            continue;
        }
        let ring_ms = (st.wrote[k] - st.sent[k]) * 1e3;
        let aaudio_ms = (st.taken[k] - st.wrote[k]) * 1e3;
        r.ring_ms += ring_ms;
        r.aaudio_ms += aaudio_ms;
        r.n += 1;
        ring_max = ring_max.max(ring_ms);
        aaudio_max = aaudio_max.max(aaudio_ms);
        r.max_ms = r.max_ms.max(ring_ms + aaudio_ms);
    }
    if r.n != 0 {
        r.ring_ms /= f64::from(r.n);
        r.aaudio_ms /= f64::from(r.n);
    }
    r.total_ms = r.ring_ms + r.aaudio_ms;
    println!(
        "stage: {name:<44} {:3} markers: socket+ring {:5.1} ms (max {ring_max:5.1}), AAudio queue {:5.1} ms (max \
         {aaudio_max:5.1}), to the server's read {:5.1} ms (max {:5.1})",
        r.n, r.ring_ms, r.aaudio_ms, r.total_ms, r.max_ms
    );
    r
}

fn speaker_case(p: &mut Player, run: &Run, ppm: f64) {
    let fake = Arc::clone(&p.fake);
    eprintln!("-- the guest's clock {ppm:+.0} ppm against the server's");
    fake.reset_late();
    run.header(AU_START, true, 2, 48000);
    p.play(run, 3.0, ppm); // the start: the start threshold's excess comes down
    let s0 = run.stats();
    p.play(run, 6.0, ppm);
    run.wait_in(9 * 48000);
    let (s, fill, aim) = {
        let r = run.helper.ring();
        (r.stats, r.count, r.target)
    };
    let dac = fake.last_out();
    let queued = dac.frames_written() - dac.frames_read();
    run.header(AU_STOP, true, 0, 0);
    // A loaded host sends the test's periods late and then back to back: the ring's overflow and its level then are
    // the host's.
    let on_time = fake.late_max() <= HOST_LATE;
    if on_time {
        assert_eq!(s.overflows, 0);
    }
    assert_eq!(s.r#in, 9 * 48000);
    assert!(dac.st().nonzero > 2 * 8 * 48000); // the tone reached the server
                                               // Drift, over the last 6 s: 2000 ppm is 576 frames there, of which the level's dead band (+-5 ms, 480 frames) may
                                               // take all but 96. So: the net slips go the right way by at least 48 frames, and at 0 ppm stay under half of 576,
                                               // and the level ends within 10 ms of its aim. These and "no underrun" need the host to run the test's threads on
                                               // time.
    let net = (s.slips_down as i64 - s0.slips_down as i64) - (s.slips_up as i64 - s0.slips_up as i64);
    if fake.host_on_time("no underrun, the slips and the level") {
        assert_eq!(s.underruns, 0);
        assert_eq!(dac.st().dac_underflows, 0);
        if ppm > 0.0 {
            assert!(net >= 48, "net slips {net}");
        }
        if ppm < 0.0 {
            assert!(net <= -48, "net slips {net}");
        }
        if ppm == 0.0 {
            assert!(net.abs() < 288, "net slips {net}");
        }
        assert!(s.margin > aim as i32 - ms_frames(10) as i32 && s.margin < aim as i32 + ms_frames(10) as i32);
    }
    if on_time {
        assert!(frames_ms((queued + i64::from(fill)) as u32) < 120); // ring + AAudio's buffer of 58 ms + a burst
    }
}

/// Latency per stage on a lingering stream: the guest starts on the helper's silence (the server's pulls known).
fn latency_case(p: &mut Player, name: &str, pull: i32, burst: i32) -> Stages {
    let fake = Arc::clone(&p.fake);
    eprintln!("-- latency: server pulls {pull} frames, burst {burst}");
    {
        let mut k = fake.k();
        (k.dac_pull, k.dac_burst, k.dac_ppm, k.dac_stall_s) = (pull, burst, 0.0, 0.0);
    }
    let run = Run::up(&fake, "60");
    fake.reset_late();
    run.header(AU_START, true, 2, 48000);
    p.play(&run, 0.5, 0.0); // the stream opens and starts; then a pause in silence
    run.header(AU_STOP, true, 0, 0);
    nap_s(0.5);
    if fake.k().hal == Hal::NoMmap {
        assert_eq!(run.helper.ring().pulls.size, pull as u32); // the pulls were seen, their size right
    }
    let mut all = Stages::default();
    for start in 0..6 {
        // six sounds, each starting at another phase of the pulls
        let first = fake.last_out().st().next_mark;
        run.header(AU_START, true, 2, 48000);
        p.play_as(&run, GuestPlay { seconds: 1.0, mark_every: 20, ..GuestPlay::default() });
        run.wait_in(48000);
        {
            let s = run.stats(); // the helper's own end to end of this sound (its STOP resets it)
            all.e2e_ms += s.e2e_sum * 1e3;
            all.late_ms += s.late_sum * 1e3;
            all.e2e_n += s.e2e_n;
        }
        run.header(AU_STOP, true, 0, 0);
        nap_s(0.31 + 0.007 * f64::from(start));
        let r = measure(&fake, name, first);
        all.ring_ms += r.ring_ms * f64::from(r.n);
        all.aaudio_ms += r.aaudio_ms * f64::from(r.n);
        all.n += r.n;
        all.max_ms = all.max_ms.max(r.max_ms);
    }
    if all.n != 0 {
        all.ring_ms /= f64::from(all.n);
        all.aaudio_ms /= f64::from(all.n);
    }
    if all.e2e_n != 0 {
        all.e2e_ms /= f64::from(all.e2e_n);
        all.late_ms /= f64::from(all.e2e_n);
    }
    all.total_ms = all.ring_ms + all.aaudio_ms;
    println!(
        "stage: {name:<44} all: socket+ring {:.1} ms, AAudio queue {:.1} ms, total {:.1} ms (max {:.1}); the helper's \
         end-to-end {:.1} ms (guest late {:.1} ms; {} periods)",
        all.ring_ms, all.aaudio_ms, all.total_ms, all.max_ms, all.e2e_ms, all.late_ms, all.e2e_n
    );
    if fake.host_on_time("no underrun") {
        assert_eq!(run.stats().underruns, 0);
        assert_eq!(fake.last_out().st().dac_underflows, 0);
    }
    if fake.late_max() <= HOST_LATE {
        assert!(all.n >= 6 * 8); // a late host loses markers to its overflows
    }
    run.down();
    p.marks_sent = 0;
    all
}

/// Plays a short sound on a fresh helper and takes it down (the open path's cases).
fn short_sound(p: &mut Player, seconds: f64) {
    let run = Run::up(&p.fake, "0");
    run.header(AU_START, true, 2, 48000);
    p.play(&run, seconds, 0.0);
    run.header(AU_STOP, true, 0, 0);
    run.down();
}

#[test]
fn audio_helper() {
    let _env = env_lock();
    for v in ["ONEIOS_AUDIO_HOST_VOLUME", "ONEIOS_AUDIO_PERF", "ONEIOS_AUDIO_MMAP", "ONEIOS_AUDIO_TARGET_DECAY_S"] {
        std::env::remove_var(v);
    }
    std::env::set_var("ONEIOS_AUDIO_SYSTEM_SERVER", "0"); // as in a session (the host's /proc is not looked at)
    let fake = Arc::new(Fake {
        knobs: Mutex::new(Knobs {
            dac_ppm: 0.0,
            dac_stall_s: 0.0,
            dac_pull: 1600,
            dac_burst: 1600,
            mmap_policy_rc: 0,
            mmap_policy_set: -1,
            fake_mmap_used: false,
            hal: Hal::NoMmap,
            mmap_burst: 192,
            mmap_capacity: 3072,
            mmap_below: 240,
            dead_mmap: false,
            media_index: 7,
            media_sets: 0,
            media_bad: 0,
            last_out: None,
            last_in: None,
            out_opens: 0,
        }),
        host_late_max: AtomicU64::new(0),
    });
    // The canary: the lateness above was measured only on the fake server's pulls and the test's sends, not on the
    // helper's own writer thread, so a loaded host (release v0.1.29's tests lane, load average 24) could pass every
    // on-time check and still give the helper an underrun (`assert_eq!(s.underruns, 0)` in speaker_case). This thread
    // wakes every 2 ms on an absolute CLOCK_MONOTONIC schedule (sleep_until), as an ordinary thread of the helper would,
    // and feeds its own lateness into the same maximum, so a host that runs any ordinary thread late skips the on-time
    // checks instead of failing them. It holds only a Weak and ends once the test drops the fake.
    {
        let weak = Arc::downgrade(&fake);
        std::thread::Builder::new()
            .name("late-canary".into())
            .spawn(move || {
                const PERIOD: f64 = 0.002;
                let mut due = time::monotonic_s() + PERIOD;
                loop {
                    sleep_until(due);
                    let now = time::monotonic_s();
                    let Some(fake) = weak.upgrade() else { break };
                    fake.late(now - due);
                    drop(fake);
                    // The next wake on the schedule; a wake that missed slots counts once, then the schedule restarts.
                    due += PERIOD;
                    if due < now {
                        due = now + PERIOD;
                    }
                }
            })
            .expect("the late canary's thread");
    }
    let k = || fake.k();
    let mut p = Player { fake: Arc::clone(&fake), marks_sent: 0 };

    // 1. Drift, one stream per case (no linger). The fake HAL has no MMAP: LOW_LATENCY (the default) asked with MMAP
    // allowed gets AAudio's own fallback, a mixer track, which is kept (one open).
    let run = Run::up(&fake, "0");
    let opens_before = k().out_opens;
    k().dac_ppm = 0.0;
    speaker_case(&mut p, &run, 0.0);
    // The host's media index on the speaker went to its maximum at the first open (and stays there for the run).
    assert!(k().media_index == 15 && k().media_sets == 1 && k().media_bad == 0);
    assert!(k().mmap_policy_set == POLICY_AUTO && fake.last_out().perf == PERF_LOW_LATENCY);
    assert_eq!(k().out_opens, opens_before + 1);
    {
        let r = run.helper.ring();
        assert!(r.out_path == OutPath::Legacy && !r.continuous && r.target_ms == 20);
    }
    k().dac_ppm = 2000.0;
    speaker_case(&mut p, &run, -2000.0); // the server 2000 ppm fast: the guest is slow against it
    k().dac_ppm = -2000.0;
    speaker_case(&mut p, &run, 2000.0);
    // The microphone: about a second of 240-frame periods comes back, in order.
    run.header(AU_START, false, 1, 48000);
    let (mut periods, mut frames, mut first) = (0u32, 0u32, 0i16);
    let t0 = time::monotonic_s();
    let mut buf = vec![0u8; AU_HEADER + AU_MAX_FRAMES as usize * 2];
    while time::monotonic_s() - t0 < 1.0 {
        // SAFETY: a valid buffer of its length.
        let got = unsafe { libc::recv(run.guest.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
        if got < AU_HEADER as isize {
            continue;
        }
        let h = AuHeader::read_from(&buf);
        if h.kind != AU_PCM {
            continue;
        }
        if periods == 0 {
            first = i16::from_le_bytes([buf[8], buf[9]]);
        }
        periods += 1;
        frames += h.value;
    }
    run.header(AU_STOP, false, 0, 0);
    assert!((180..=210).contains(&periods), "{periods} periods");
    assert!(frames == periods * 240 && first == 100);
    assert_eq!(k().last_in.clone().unwrap().perf, PERF_NONE); // the microphone stays NONE
    assert_eq!(k().media_sets, 1); // one set a run, not one per stream
    run.down();
    assert!(k().media_index == 7 && k().media_sets == 2 && k().media_bad == 0); // put back as the helper ended
                                                                                // ONEIOS_AUDIO_HOST_VOLUME=keep: the index is not touched.
    std::env::set_var("ONEIOS_AUDIO_HOST_VOLUME", "keep");
    short_sound(&mut p, 0.2);
    assert!(k().media_index == 7 && k().media_sets == 2);
    std::env::remove_var("ONEIOS_AUDIO_HOST_VOLUME");
    // Already at the maximum: nothing set, nothing put back.
    k().media_index = 15;
    short_sound(&mut p, 0.2);
    assert!(k().media_index == 15 && k().media_sets == 2);
    k().media_index = 7;

    // 2. Linger: a START soon after a STOP takes the open stream (no new open, no stall); the stream closes after the
    // silence.
    eprintln!("-- linger 1 s; the new track stalls 200 ms");
    (k().dac_ppm, k().dac_stall_s) = (0.0, 0.2);
    let run = Run::up(&fake, "1");
    fake.reset_late();
    run.header(AU_START, true, 2, 48000);
    p.play(&run, 1.0, 0.0);
    run.wait_in(48000);
    let kept = {
        let s = run.stats();
        s.overflows == 0 && s.dropped == 0 // the stall's audio waits in the ring, none is cut
    };
    if fake.host_on_time("nothing cut after the stall") {
        assert!(kept);
    }
    run.header(AU_STOP, true, 0, 0);
    let mut tail = false; // the tail plays after the STOP, nothing is cleared
    for _ in 0..100 {
        nap_s(0.02);
        let r = run.helper.ring();
        if r.stats.played + r.stats.dropped == r.stats.r#in && r.count == 0 {
            tail = true;
            break;
        }
    }
    assert!(tail);
    let first_stream = fake.last_out();
    assert!(run.helper.streams_open().0);
    fake.reset_late();
    run.header(AU_START, true, 2, 48000);
    p.play(&run, 1.0, 0.0);
    run.wait_in(48000);
    assert!(Arc::ptr_eq(&fake.last_out(), &first_stream));
    let s = run.stats();
    assert!(s.overflows == 0 && s.r#in == 48000);
    if fake.host_on_time("no underrun on the lingering stream") {
        assert!(s.underruns == 0 && first_stream.st().dac_underflows == 0);
    }
    run.header(AU_STOP, true, 0, 0);
    nap_s(2.6);
    assert!(!run.helper.streams_open().0);
    run.down();

    // 3. A new track's stall: 300 ms of sound, then iOS's engine runs on with digital silence. The backlog of the stall
    // goes at the first silence (not over 16 s of faster playing): a marker 1 s later is on time.
    eprintln!("-- a new track stalls 200 ms, then the sound is followed by silence");
    k().dac_stall_s = 0.2;
    let run = Run::up(&fake, "0");
    fake.reset_late();
    run.header(AU_START, true, 2, 48000);
    p.play(&run, 0.3, 0.0);
    p.play_as(&run, GuestPlay { seconds: 1.0, silence: true, ..GuestPlay::default() });
    let stall_first = fake.last_out().st().next_mark;
    p.play_as(&run, GuestPlay { seconds: 1.0, silence: true, mark_every: 20, ..GuestPlay::default() });
    let after_stall = measure(&fake, "after a 200 ms stall, then 1 s of silence", stall_first);
    let (silence_cut, overflows) = {
        let s = run.stats();
        (s.silence, s.overflows)
    };
    run.header(AU_STOP, true, 0, 0);
    assert!(after_stall.n >= 8);
    if fake.host_on_time("the stall's silence cut and the latency after it") {
        assert!(silence_cut >= u64::from(ms_frames(150))); // most of the stall's 200 ms went as silence
        assert_eq!(overflows, 0);
        assert!(after_stall.total_ms < 80.0); // on time again (268 ms before)
    }
    run.down();
    p.marks_sent = 0;

    // 4. A guest whose vCPU is late by 40 ms every 0.5 s (more than the target): underruns raise the target, and it
    // comes back down to its floor (ONEIOS_AUDIO_TARGET_DECAY_S=1 here) once the guest is on time.
    eprintln!("-- the guest late by 40 ms every 0.5 s, then on time");
    k().dac_stall_s = 0.0;
    std::env::set_var("ONEIOS_AUDIO_TARGET_DECAY_S", "1");
    let run = Run::up(&fake, "0");
    run.header(AU_START, true, 2, 48000);
    p.play(&run, 3.0, 0.0); // the start's excess gone: the level at its aim
    p.play_as(&run, GuestPlay { seconds: 2.0, late_every: 100, late: 8, ..GuestPlay::default() });
    let (peak, late_underruns) = {
        let s = run.stats();
        (s.target_peak, s.underruns)
    };
    fake.reset_late();
    p.play(&run, 5.0, 0.0);
    let settled = run.helper.ring().target;
    eprintln!(
        "   {late_underruns} underruns while late; the target up to {} ms, {} ms after 5 s on time",
        frames_ms(peak),
        frames_ms(settled)
    );
    assert!(late_underruns >= 1 && peak > ms_frames(20));
    if fake.host_on_time("the target back at its floor") {
        assert_eq!(settled, ms_frames(20));
    }
    run.header(AU_STOP, true, 0, 0);
    run.down();
    std::env::remove_var("ONEIOS_AUDIO_TARGET_DECAY_S");

    // 5. Latency per stage, the guest starting at six phases of the server's pulls. The phone's shape: pulls of 1600
    // frames (33 ms) and a burst of 1600; then a server that pulls 800 (17 ms) with the same burst.
    let phone = latency_case(&mut p, "pulls 1600, burst 1600 (the phone's)", 1600, 1600);
    if fake.host_on_time("the latency bounds") {
        assert!(phone.aaudio_ms < 20.0 + 33.3 / 2.0 + 8.0); // the target plus half a pull, and a little
        assert!(phone.ring_ms < 8.0);
    }
    let half = latency_case(&mut p, "pulls 800, burst 1600", 800, 1600);
    if fake.host_on_time("the latency bounds") {
        assert!(half.aaudio_ms < 20.0 + 16.7 / 2.0 + 8.0); // the burst no longer counts: the pull does
    }

    // 6. AAudio_setMMapPolicy fails: a mixer track LOW_LATENCY (AAudio's own fallback) is kept; a mixer track that says
    // MMAP (shared, no system_server): closed, and with MMAP not refusable opened again NONE. A track that says MMAP
    // with MMAP refused: opened again NONE.
    eprintln!("-- MMAP policy not settable; MMAP in use anyway");
    (k().dac_pull, k().dac_burst) = (1600, 1600);
    k().mmap_policy_rc = -1;
    let opens = k().out_opens;
    let run = Run::up(&fake, "0");
    run.header(AU_START, true, 2, 48000);
    p.play(&run, 0.2, 0.0);
    assert!(k().out_opens == opens + 1 && fake.last_out().perf == PERF_LOW_LATENCY);
    assert_eq!(run.helper.ring().out_path, OutPath::Legacy);
    run.header(AU_STOP, true, 0, 0);
    run.down();
    k().fake_mmap_used = true;
    let opens = k().out_opens;
    short_sound(&mut p, 0.2);
    assert!(k().out_opens == opens + 2 && fake.last_out().perf == PERF_NONE);
    k().mmap_policy_rc = 0;
    let opens = k().out_opens;
    short_sound(&mut p, 0.2);
    assert!(k().out_opens == opens + 3 && fake.last_out().perf == PERF_NONE && k().mmap_policy_set == POLICY_NEVER);
    k().fake_mmap_used = false;

    // 7. ONEIOS_AUDIO_PERF=none: NONE (the path before 2026-11-15); ONEIOS_AUDIO_MMAP=off: LOW_LATENCY, MMAP refused.
    std::env::set_var("ONEIOS_AUDIO_PERF", "none");
    short_sound(&mut p, 0.2);
    assert!(fake.last_out().perf == PERF_NONE && k().mmap_policy_set == POLICY_NEVER);
    std::env::remove_var("ONEIOS_AUDIO_PERF");
    std::env::set_var("ONEIOS_AUDIO_MMAP", "off");
    k().hal = Hal::MmapExclusive;
    let run = Run::up(&fake, "0");
    run.header(AU_START, true, 2, 48000);
    p.play(&run, 0.2, 0.0);
    let last = fake.last_out();
    assert!(last.perf == PERF_LOW_LATENCY && !last.mmap && k().mmap_policy_set == POLICY_NEVER);
    assert_eq!(run.helper.ring().out_path, OutPath::Legacy);
    run.header(AU_STOP, true, 0, 0);
    run.down();
    std::env::remove_var("ONEIOS_AUDIO_MMAP");

    // 8. MMAP exclusive (the HAL has it): the DSP reads a burst of 192 every 4 ms from the start, and presents a frame
    // 5 ms after its read. One open, the level 5 ms above a chunk, no xrun, the drift steered.
    let burst = k().mmap_burst; // one lock at a time: two guards of one statement would deadlock
    let below = k().mmap_below;
    eprintln!("-- MMAP exclusive: burst {burst}, {below} frames below the read");
    k().hal = Hal::MmapExclusive;
    let run = Run::up(&fake, "0");
    let opens = k().out_opens;
    k().dac_ppm = 0.0;
    speaker_case(&mut p, &run, 0.0);
    let last = fake.last_out();
    assert!(
        k().out_opens == opens + 1
            && last.mmap
            && last.sharing == SHARING_EXCLUSIVE
            && k().mmap_policy_set == POLICY_AUTO
    );
    {
        let r = run.helper.ring();
        assert!(r.out_path == OutPath::MmapExclusive && r.continuous && r.target_ms == 5);
    }
    k().dac_ppm = 2000.0;
    speaker_case(&mut p, &run, -2000.0);
    k().dac_ppm = -2000.0;
    speaker_case(&mut p, &run, 2000.0);
    k().dac_ppm = 0.0;
    run.down();
    let mmap_x = latency_case(&mut p, "MMAP exclusive, burst 192", 1600, 1600);
    if fake.host_on_time("the MMAP latency bounds") {
        assert!(mmap_x.aaudio_ms < 10.0 + 2.5 + 4.0 + 3.0); // the start's target (10 ms), half a chunk, a burst, a little
        assert!(mmap_x.ring_ms < 3.0);
        // below the read 5 ms + the level + lateness
        assert!(mmap_x.e2e_n > 100 && mmap_x.e2e_ms > 5.0 && mmap_x.e2e_ms < 5.0 + 10.0 + 4.0 + 6.0);
    }

    // 9. MMAP shared offered: without system_server closed before its start, a mixer track instead; with it kept.
    eprintln!("-- MMAP shared, without and with system_server");
    k().hal = Hal::MmapShared;
    let opens = k().out_opens;
    let run = Run::up(&fake, "0");
    run.header(AU_START, true, 2, 48000);
    p.play(&run, 0.3, 0.0);
    assert!(k().out_opens == opens + 2 && !fake.last_out().mmap && k().mmap_policy_set == POLICY_NEVER);
    assert_eq!(run.helper.ring().out_path, OutPath::Legacy);
    run.header(AU_STOP, true, 0, 0);
    run.down();
    std::env::set_var("ONEIOS_AUDIO_SYSTEM_SERVER", "1");
    let media_sets = k().media_sets; // beside One UI the host's media index is not touched
    let opens = k().out_opens;
    let run = Run::up(&fake, "0");
    fake.reset_late();
    run.header(AU_START, true, 2, 48000);
    p.play(&run, 2.0, 0.0);
    run.wait_in(2 * 48000);
    let last = fake.last_out();
    assert!(k().out_opens == opens + 1 && last.mmap && last.sharing == SHARING_SHARED);
    assert_eq!(run.helper.ring().out_path, OutPath::MmapShared);
    let shared_underruns = run.stats().underruns;
    if fake.host_on_time("no underrun on MMAP shared") {
        assert!(shared_underruns == 0 && last.st().dac_underflows == 0);
    }
    run.header(AU_STOP, true, 0, 0);
    run.down();
    assert!(k().media_sets == media_sets && k().media_index == 7);
    std::env::set_var("ONEIOS_AUDIO_SYSTEM_SERVER", "0");

    // 10. The probe (xnuhost before the boot): the speaker opened in silence, its latency answered (below the read plus
    // the target: 240 + 240 frames here), the stream kept for the guest's first START (no second open); the output
    // latency iOS is told comes back in the end-to-end line.
    eprintln!("-- the probe");
    k().hal = Hal::MmapExclusive;
    let run = Run::up(&fake, "60");
    let opens = k().out_opens;
    fake.reset_late();
    run.header(AU_PROBE, true, 0, 0);
    let mut answer = AuHeader::default();
    let t0 = time::monotonic_s();
    while time::monotonic_s() - t0 < 3.0 && answer.kind != AU_LATENCY {
        let mut wait = libc::pollfd { fd: run.guest.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(&mut wait, 1, 100) } > 0 {
            let mut b = [0u8; AU_HEADER];
            // SAFETY: a valid buffer of its length.
            let got = unsafe { libc::recv(run.guest.as_raw_fd(), b.as_mut_ptr().cast(), b.len(), 0) };
            answer = if got < AU_HEADER as isize { AuHeader::default() } else { AuHeader::read_from(&b) };
        }
    }
    eprintln!("   probe: {} frames", answer.value);
    // A late host makes the silence's writes late, AAudio's xruns raise the target, and the probe says so (+5 ms each).
    assert!(answer.kind == AU_LATENCY && answer.value >= 400 && answer.value <= 240 + ms_frames(80));
    assert_eq!(k().out_opens, opens + 1);
    let probed_target = run.helper.ring().target;
    assert!((i64::from(answer.value) - i64::from(240 + probed_target)).abs() <= 96); // below the read + the level
    run.header(AU_TOLD, true, 0, answer.value);
    run.header(AU_START, true, 2, 48000);
    p.play(&run, 1.0, 0.0);
    run.wait_in(48000);
    assert!(k().out_opens == opens + 1 && run.helper.told_frames() == answer.value);
    run.header(AU_STOP, true, 0, 0);
    run.down();

    // 11. MMAP exclusive, the guest late by 40 ms every 0.5 s: underruns (the level's floor of 5 ms is far below) raise
    // the target; on time again it comes back to 5 ms.
    eprintln!("-- MMAP exclusive: the guest late by 40 ms every 0.5 s, then on time");
    std::env::set_var("ONEIOS_AUDIO_TARGET_DECAY_S", "1");
    let run = Run::up(&fake, "0");
    run.header(AU_START, true, 2, 48000);
    p.play(&run, 1.0, 0.0);
    p.play_as(&run, GuestPlay { seconds: 2.0, late_every: 100, late: 8, ..GuestPlay::default() });
    let (peak, late_underruns) = {
        let s = run.stats();
        (s.target_peak, s.underruns)
    };
    fake.reset_late();
    p.play(&run, 5.0, 0.0);
    let settled = run.helper.ring().target;
    eprintln!(
        "   {late_underruns} underruns while late; the target up to {} ms, {} ms after 5 s on time",
        frames_ms(peak),
        frames_ms(settled)
    );
    assert!(late_underruns >= 1 && peak > ms_frames(10)); // the stream starts at 10 ms; the late guest raised it
                                                          // On time again it comes down as far as the guest's lateness allows (5 ms needs periods within 2 ms).
    if fake.host_on_time("the MMAP target back down") {
        assert!(settled < peak && settled <= ms_frames(10));
        if fake.late_max() < 0.0015 {
            assert_eq!(settled, ms_frames(5));
        }
    }
    run.header(AU_STOP, true, 0, 0);
    run.down();
    std::env::remove_var("ONEIOS_AUDIO_TARGET_DECAY_S");

    // 12. An MMAP exclusive stream whose server takes no frames (the S25's HAL: the DSP never started, no timestamp,
    // every write times out with 0). The probe reopens the speaker as a mixer track before the guest's boot; the
    // writer's watchdog (three timed-out writes, 600 ms) does the same in the middle of a run. After either, no open
    // asks for MMAP again.
    eprintln!("-- an MMAP stream whose server takes no frames: the mixer track instead");
    k().hal = Hal::MmapExclusive;
    k().dead_mmap = true;
    let run = Run::up(&fake, "60");
    let opens = k().out_opens;
    run.header(AU_PROBE, true, 0, 0);
    let mut answer = AuHeader::default();
    let t0 = time::monotonic_s();
    while time::monotonic_s() - t0 < 6.0 && answer.kind != AU_LATENCY {
        let mut wait = libc::pollfd { fd: run.guest.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(&mut wait, 1, 100) } > 0 {
            let mut b = [0u8; AU_HEADER];
            // SAFETY: a valid buffer of its length.
            let got = unsafe { libc::recv(run.guest.as_raw_fd(), b.as_mut_ptr().cast(), b.len(), 0) };
            answer = if got < AU_HEADER as isize { AuHeader::default() } else { AuHeader::read_from(&b) };
        }
    }
    eprintln!("   probe: {} frames", answer.value);
    // The mixer track starts on the guest's first period, so the probe's silence has no timestamp: not measured (0).
    // What this case is about is the reopen.
    assert!(answer.kind == AU_LATENCY && answer.value == 0);
    assert_eq!(k().out_opens, opens + 2); // the MMAP stream, then the mixer track
    assert!(!fake.last_out().mmap && k().mmap_policy_set == POLICY_NEVER);
    assert_eq!(run.helper.ring().out_path, OutPath::Legacy);
    run.header(AU_START, true, 2, 48000);
    p.play(&run, 0.5, 0.0);
    nap_s(0.1);
    assert!(run.stats().played > 0); // the guest's sound plays on the mixer track
    run.header(AU_STOP, true, 0, 0);
    run.down();

    // A stream that dies in the middle: a working MMAP stream, then its DSP stops taking frames while the guest's
    // stream is up (no STOP: the guest's stream is kept). The writer notices in about 1 s, the receive loop reopens the
    // speaker as a mixer track and the next period plays on it without a new START.
    k().dead_mmap = false;
    let run = Run::up(&fake, "60");
    let opens = k().out_opens;
    run.header(AU_START, true, 2, 48000);
    p.play(&run, 0.3, 0.0);
    assert!(fake.last_out().mmap && k().out_opens == opens + 1);
    let played = run.stats().played;
    assert!(played > 0);
    k().dead_mmap = true;
    let t0 = time::monotonic_s();
    while time::monotonic_s() - t0 < 3.0 && k().out_opens < opens + 2 {
        p.play(&run, 0.05, 0.0);
    }
    assert!(k().out_opens == opens + 2 && !fake.last_out().mmap);
    assert_eq!(run.helper.ring().out_path, OutPath::Legacy);
    assert!(run.helper.ring().playing()); // the guest's stream was not stopped: its periods still play
    let played = run.stats().played; // the new stream's ring starts its counts again
    p.play(&run, 0.5, 0.0);
    nap_s(0.1);
    assert!(run.stats().played > played);
    run.header(AU_STOP, true, 0, 0);
    run.down();
    k().dead_mmap = false;

    // A stream that is dead from its first write (no probe before it, as a later START after a new open): the write's
    // 600 ms of timeouts reopen the speaker; the guest's first sound then plays on the mixer track.
    let run = Run::up(&fake, "60");
    let opens = k().out_opens;
    k().dead_mmap = true;
    run.header(AU_START, true, 2, 48000);
    let t0 = time::monotonic_s();
    while time::monotonic_s() - t0 < 3.0 && k().out_opens < opens + 2 {
        p.play(&run, 0.05, 0.0);
    }
    assert!(k().out_opens == opens + 2 && !fake.last_out().mmap);
    assert_eq!(run.helper.ring().out_path, OutPath::Legacy);
    assert!(run.helper.ring().playing());
    p.play(&run, 0.5, 0.0);
    nap_s(0.1);
    assert!(run.stats().played > 0);
    run.header(AU_STOP, true, 0, 0);
    run.down();
    k().dead_mmap = false;
    std::env::remove_var("ONEIOS_AUDIO_SYSTEM_SERVER");
    std::env::remove_var("ONEIOS_AUDIO_LINGER_S");
}

#[test]
fn resample_slips() {
    // A slip is interpolated; a short chunk is padded; equal lengths are copied.
    let input: Vec<i16> = (0..480).map(|i| (i / 2 * 10) as i16).collect(); // 240 stereo frames: 0, 10, 20, ...
    let mut out = vec![0i16; 2 * 244];
    resample(&mut out, 240, &input, 240);
    assert_eq!(out[..480], input[..]);
    resample(&mut out, 239, &input, 240);
    assert!(out[0] == 0 && out[2 * 238] == 2390 && out[2 * 100] > 1000);
    resample(&mut out, 241, &input, 240);
    assert!(out[0] == 0 && out[2 * 240] == 2390);
    resample(&mut out, 240, &input, 100);
    assert!(out[2 * 99] == 990 && out[2 * 100] == 0 && out[2 * 239] == 0);
}
