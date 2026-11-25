// SPDX-License-Identifier: MIT
//! The backend interface (encoder.h): one encoder session per virtio-media session, fed NV12 or P010 frames, giving
//! coded access units back. Backends (both samsung and treble use the same path: Android's MediaCodec):
//! - [`crate::backend_ndk`]: the NDK's AMediaCodec (video/hevc -> c2.qti.hevc.encoder, video/avc); the phone and
//!   `encctl`.
//! - [`crate::backend_stub`]: the stand-in: no MediaCodec on the box, so it writes a real HEVC/H.264 Annex-B access
//!   unit of the frame's own samples (the host tests, xnuhost's encoder=sim and its PC boots).
//! - [`crate::backend_helper`]: the device-side backend that runs the real encoder in the oneios-encoder helper
//!   process.
//!
//! 10-bit pictures (2026-11-23): the guest sends iOS's 10-bit surfaces (x420, xf20) as P010. A backend codes them as
//! HEVC Main10 when its encoder offers Main10 at that size, and else as 8-bit (each sample's high byte, NV12): the
//! session says which ([`Coding`]).
use crate::proto::{MEDIA_PIXFMT_NV12, MEDIA_PIXFMT_P010};

/// What a poll gave: `bytes` of one coded access unit (0: none ready yet), whether it is a keyframe, its pts, and
/// whether the last buffer has come out after a drain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Polled {
    pub bytes: usize,
    pub key: bool,
    pub pts_us: i64,
    pub eos: bool,
}

/// How a session codes its pictures: 8-bit from NV12, HEVC Main10 from P010, or P010 coded 8-bit (its samples' high
/// bytes: the encoder offers no Main10 at that size). The number is the helper link's (`ENC_MSG_OPEN_REPLY`'s b).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Coding {
    #[default]
    Eight = 0,
    Main10 = 1,
    TenAsEight = 2,
}

impl Coding {
    pub fn from_u32(v: u32) -> Coding {
        match v {
            1 => Coding::Main10,
            2 => Coding::TenAsEight,
            _ => Coding::Eight,
        }
    }

    /// The words of the session's log line.
    pub fn text(self) -> &'static str {
        match self {
            Coding::Eight => "8-bit",
            Coding::Main10 => "10-bit pictures as Main10 (P010)",
            Coding::TenAsEight => "10-bit pictures as 8-bit (the encoder offers no Main10 at this size)",
        }
    }
}

/// The device's worker's wake-up, which a session calls when a coded unit (or a drain's end) is ready for its poll.
pub type Wakeup = std::sync::Arc<dyn Fn() + Send + Sync>;

/// One encoder session on a backend (`br_enc_session`). Dropping it closes it.
pub trait Session: Send {
    /// Feeds one frame, tightly packed in the session's input format (NV12: w*h luma + w*h/2 chroma; P010: the same
    /// with two bytes a sample); pts in microseconds; `force_key` asks for an IDR. The backend copies what it needs:
    /// the caller may reuse the buffer at once. An errno on failure.
    fn feed(&mut self, frame: &[u8], pts_us: i64, force_key: bool) -> Result<(), i32>;
    /// Asks the backend to finish every frame fed so far (end of stream).
    fn drain(&mut self) -> Result<(), i32>;
    /// One coded access unit, Annex-B (start codes), into `buf`, or an errno.
    fn poll(&mut self, buf: &mut [u8]) -> Result<Polled, i32>;
    /// How this session codes its pictures.
    fn coding(&self) -> Coding {
        Coding::Eight
    }
    /// The device's worker hands its wake-up over once, when the session opens (2026-11-25). A session that keeps it
    /// and calls it each time a poll would find something (a coded unit, a drain's end, its helper gone) says true;
    /// so does one whose units are ready only after a `feed` or `drain`, which come in a guest's command, and a
    /// command wakes the worker anyway (the stand-in). The worker then sleeps until it is woken, its guard at most
    /// (`media_device::MEDIA_IDLE_MS`). False, the default (the NDK's MediaCodec in-process: encctl): the worker asks
    /// every millisecond for `media_device::MEDIA_BUSY_MS` after each command, as it did for every backend before.
    fn wakes(&mut self, wake: Wakeup) -> bool {
        let _ = wake;
        false
    }
}

/// A backend (`br_encoder_backend`).
pub trait Backend: Send + Sync {
    fn name(&self) -> &'static str;
    /// A session for `codec` (MEDIA_PIXFMT_HEVC or MEDIA_PIXFMT_H264) at width x height, its input tightly packed
    /// `input` (MEDIA_PIXFMT_NV12 or MEDIA_PIXFMT_P010); None when it is refused.
    fn open(&self, codec: u32, input: u32, width: u32, height: u32) -> Option<Box<dyn Session>>;
    /// A guest opened a virtio-media session (a vCPU calls it): the backend may get ready for an encoder's open (the
    /// helper backend starts its helper, which loads MediaCodec then). Never waits long.
    fn prepare(&self) {}
}

/// Whether `input` is a format the device takes on its OUTPUT queue.
pub fn input_format(input: u32) -> bool {
    input == MEDIA_PIXFMT_NV12 || input == MEDIA_PIXFMT_P010
}

/// The high byte of each 16-bit little-endian sample of `p010` (a tightly packed P010 picture) into `nv12`, the
/// 8-bit picture of the same size: the 10-bit value's top 8 bits. `nv12` takes `p010.len() / 2` bytes.
pub fn p010_to_nv12(p010: &[u8], nv12: &mut [u8]) {
    for (to, sample) in nv12.iter_mut().zip(p010.chunks_exact(2)) {
        *to = sample[1];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_bytes() {
        // 10-bit 1023 (0xffc0), 512 (0x8000), 4 (0x0100), 3 (0x00c0): high bytes 255, 128, 1, 0
        let p010 = [0xc0, 0xff, 0x00, 0x80, 0x00, 0x01, 0xc0, 0x00];
        let mut nv12 = [9u8; 4];
        p010_to_nv12(&p010, &mut nv12);
        assert_eq!(nv12, [255, 128, 1, 0]);
        assert_eq!(Coding::from_u32(Coding::Main10 as u32), Coding::Main10);
        assert_eq!(Coding::from_u32(Coding::TenAsEight as u32), Coding::TenAsEight);
        assert_eq!(Coding::from_u32(7), Coding::Eight);
    }
}
