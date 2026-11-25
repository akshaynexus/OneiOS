// SPDX-License-Identifier: MIT
//! The media device's worker waits for an encoder's units instead of asking every millisecond (fix/codec-irq,
//! 2026-11-25). Each test streams 2 s of 30 frames a second through one encoder session as DroidVMVTHost.cpp does
//! (QBUF a frame, DQBUF its coded unit, QBUF the CAPTURE buffer again) and counts the worker's wake-ups by why
//! (`media_device::wakes`, the `media: 30 s` line's counts):
//! - a backend like MediaCodec, whose units come a few ms after their frame on a thread of its own and which wakes the
//!   worker with each: no 1 ms looks, a wake-up per command or unit and its guard's few;
//! - the same backend unable to wake it (the NDK's in-process MediaCodec: encctl): the old path, about 2,000 1 ms
//!   looks in the 2 s, every unit still out;
//! - the oneios-encoder helper process (the phone's arrangement, the stand-in in MediaCodec's place): no 1 ms looks.
use oneencoder::backend::{Backend, Polled, Session, Wakeup};
use oneencoder::backend_helper::HelperBackend;
use oneencoder::encctl::{querybuf, Guest};
use oneencoder::media_device::{self, Wakes, MEDIA_IDLE_MS};
use oneencoder::pod::Pod;
use oneencoder::proto::*;
use std::collections::VecDeque;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const W: u32 = 64;
const H: u32 = 64;
const FRAMES: u32 = 60;
const FRAME_MS: u64 = 33;
/// MediaCodec's lag: a unit comes this long after its frame
const LAG_MS: u64 = 4;

/// A backend like the helper's MediaCodec: each frame's unit comes [`LAG_MS`] later on a thread of the session's,
/// which wakes the worker when `wakes` (else the worker has to look).
struct Lagging {
    wakes: bool,
}

struct LaggingSession {
    wakes: bool,
    frames: Option<Sender<i64>>,
    ready: Arc<Mutex<VecDeque<i64>>>,
    wake: Arc<Mutex<Option<Wakeup>>>,
}

impl Backend for Lagging {
    fn name(&self) -> &'static str {
        "lagging"
    }

    fn open(&self, _codec: u32, _input: u32, _width: u32, _height: u32) -> Option<Box<dyn Session>> {
        let (frames, rx) = channel::<i64>();
        let ready: Arc<Mutex<VecDeque<i64>>> = Arc::default();
        let wake: Arc<Mutex<Option<Wakeup>>> = Arc::default();
        let (r, w) = (Arc::clone(&ready), Arc::clone(&wake));
        std::thread::spawn(move || {
            while let Ok(pts) = rx.recv() {
                std::thread::sleep(Duration::from_millis(LAG_MS));
                r.lock().unwrap().push_back(pts);
                if let Some(wake) = w.lock().unwrap().as_ref() {
                    wake();
                }
            }
        });
        Some(Box::new(LaggingSession { wakes: self.wakes, frames: Some(frames), ready, wake }))
    }
}

impl Session for LaggingSession {
    fn feed(&mut self, _frame: &[u8], pts_us: i64, _force_key: bool) -> Result<(), i32> {
        self.frames.as_ref().unwrap().send(pts_us).map_err(|_| libc::EIO)
    }

    fn drain(&mut self) -> Result<(), i32> {
        Ok(())
    }

    fn poll(&mut self, buf: &mut [u8]) -> Result<Polled, i32> {
        let Some(pts) = self.ready.lock().unwrap().pop_front() else { return Ok(Polled::default()) };
        buf[..5].copy_from_slice(&[0, 0, 0, 1, 0x26]); // a start code and an IDR's NAL header: a unit of 5 bytes
        Ok(Polled { bytes: 5, key: true, pts_us: pts, eos: false })
    }

    fn wakes(&mut self, wake: Wakeup) -> bool {
        if self.wakes {
            *self.wake.lock().unwrap() = Some(wake);
        }
        self.wakes
    }
}

impl Drop for LaggingSession {
    fn drop(&mut self) {
        self.frames = None; // its thread ends
    }
}

/// An encoder session streaming [`FRAMES`] frames, one every [`FRAME_MS`], each unit taken before the next frame. The
/// worker's wake-ups in the stream, and the units out.
fn stream(backend: Arc<dyn Backend>, base: u64) -> (Wakes, u32) {
    let mut g = Guest::up(backend, base).expect("the device and its guest");
    let session = g.open().expect("OPEN");
    let mut cap = MediaV4l2Format {
        type_: MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE,
        width: W,
        height: H,
        pixelformat: MEDIA_PIXFMT_HEVC,
        num_planes: 1,
        ..Default::default()
    };
    assert_eq!(g.ioctl(session, MEDIA_VIDIOC_S_FMT, cap.as_bytes_mut()), 0);
    let mut out = MediaV4l2Format {
        type_: MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE,
        width: W,
        height: H,
        pixelformat: MEDIA_PIXFMT_NV12,
        num_planes: 1,
        ..Default::default()
    };
    assert_eq!(g.ioctl(session, MEDIA_VIDIOC_S_FMT, out.as_bytes_mut()), 0);
    let mut req = [2, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, MEDIA_MEMORY_MMAP, 0, 0];
    assert!(g.ioctl(session, MEDIA_VIDIOC_REQBUFS, req.as_bytes_mut()) == 0 && req[0] == 2);
    let mut frames = Vec::new();
    for i in 0..2 {
        let b = querybuf(&mut g, session, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, i);
        frames.push(g.mmap(session, b.plane.m).expect("MMAP of an OUTPUT buffer").0);
    }
    let mut req = [4, MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, MEDIA_MEMORY_MMAP, 0, 0];
    assert!(g.ioctl(session, MEDIA_VIDIOC_REQBUFS, req.as_bytes_mut()) == 0 && req[0] == 4);
    for i in 0..4 {
        let mut cb = querybuf(&mut g, session, MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, i);
        assert!(g.mmap(session, cb.plane.m).is_ok());
        assert_eq!(g.ioctl(session, MEDIA_VIDIOC_QBUF, cb.as_bytes_mut()), 0);
    }
    assert_eq!(g.stream_on(session, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE), 0);
    assert_eq!(g.stream_on(session, MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE), 0);

    let picture: Vec<u8> = (0..(W * H * 3 / 2) as usize).map(|i| (i * 7 + 13) as u8).collect();
    let from = media_device::wakes(&g.dev);
    let start = Instant::now();
    let mut coded = 0;
    for n in 0..FRAMES {
        let index = n % 2;
        g.write(frames[index as usize], &picture);
        let mut qb = MediaV4l2Buffer {
            type_: MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE,
            index,
            sec: i64::from(n),
            ..Default::default()
        };
        qb.plane.bytesused = picture.len() as u32;
        assert_eq!(g.ioctl(session, MEDIA_VIDIOC_QBUF, qb.as_bytes_mut()), 0);
        // its OUTPUT buffer back, then its unit; the CAPTURE buffer queued again
        let mut got = false;
        while !got {
            let (kind, _, ev) = g.wait_event(Duration::from_secs(3)).expect("the frame's events");
            assert_eq!(kind, MEDIA_EVT_DQBUF);
            let b = MediaV4l2Buffer::read_from(&ev[8..]);
            if b.type_ == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE {
                assert!(b.plane.bytesused > 0);
                let mut again = querybuf(&mut g, session, MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, b.index);
                assert_eq!(g.ioctl(session, MEDIA_VIDIOC_QBUF, again.as_bytes_mut()), 0);
                (got, coded) = (true, coded + 1);
            }
        }
        let next = Duration::from_millis(FRAME_MS * u64::from(n + 1));
        if let Some(left) = next.checked_sub(start.elapsed()) {
            std::thread::sleep(left); // the next frame's time
        }
    }
    let now = media_device::wakes(&g.dev);
    assert_eq!(g.close(session), Some(0));
    let wakes = Wakes {
        woken: now.woken - from.woken,
        timed_out: now.timed_out - from.timed_out,
        ticks: now.ticks - from.ticks,
    };
    (wakes, coded)
}

/// What a worker that waits may cost in the 2 s: a wake-up per command (three a frame: QBUF, QUERYBUF, QBUF), per
/// kick (the two event buffers given back) and per unit, its guard's ([`MEDIA_IDLE_MS`]) at most once a guard, and no
/// 1 ms look (the old path: about 2,000).
fn waited(w: &Wakes) -> bool {
    let commands_and_units = u64::from(FRAMES) * 6;
    let guards = FRAME_MS * u64::from(FRAMES) / MEDIA_IDLE_MS + 5;
    w.ticks == 0 && w.woken >= u64::from(FRAMES) && w.timed_out <= guards && w.total() <= commands_and_units + guards
}

#[test]
fn the_worker_waits_for_a_backend_that_wakes_it() {
    let (w, coded) = stream(Arc::new(Lagging { wakes: true }), 0x0c00_0000);
    eprintln!("the worker in the 2 s: {w:?}, {coded} units");
    assert_eq!(coded, FRAMES);
    assert!(waited(&w), "{w:?}");
}

#[test]
fn a_backend_that_cannot_wake_it_is_still_asked_every_millisecond() {
    let (w, coded) = stream(Arc::new(Lagging { wakes: false }), 0x0c00_1000);
    eprintln!("the worker in the 2 s: {w:?}, {coded} units");
    assert_eq!(coded, FRAMES, "every unit out on the old path too");
    // 2 s of 1 ms looks (each a sleep of 1 ms at least): far more than a waiting worker's few hundred wake-ups
    assert!(w.ticks >= 1000, "{w:?}");
}

#[test]
fn the_helper_process_wakes_the_worker() {
    let (w, coded) = stream(HelperBackend::new(env!("CARGO_BIN_EXE_oneios-encoder")), 0x0c00_2000);
    eprintln!("the worker in the 2 s: {w:?}, {coded} units");
    assert_eq!(coded, FRAMES);
    assert!(waited(&w), "{w:?}");
}
