// SPDX-License-Identifier: MIT
//! The Android encoder backend (backend_ndk.c): the NDK's AMediaCodec. "video/hevc" reaches Codec2's
//! c2.qti.hevc.encoder on the S24 Ultra, "video/avc" the AVC encoder; both the samsung and treble hardware backends use
//! this same path, so one file serves both. ByteBuffer input (NV12, COLOR_FormatYUV420SemiPlanar; or P010,
//! COLOR_FormatYUVP010, for HEVC Main10); the output is an Annex-B byte stream (start codes), which is what
//! DroidVMVTHost.cpp and CMPhoto expect.
//!
//! 10-bit pictures (2026-11-23): a session of P010 frames is opened as HEVC Main10 with P010 input when the encoder
//! offers it at that size (its capabilities say so, or none can be read and the configure is tried), first at constant
//! quality, then at a bit rate; else as 8-bit, each sample's high byte into NV12 ([`Coding`]). A size refused for
//! Main10 is remembered by the process and not tried again.
//!
//! The codec's size (2026-11-23): with Android 16's codec capabilities (AMediaCodecStore, API 36, found with dlsym:
//! the programs are built for API 30), the encoder is the first vendor encoder of the type, opened by its name, and a
//! picture smaller than its smallest size, or not of its alignment, is coded at the next size it takes, the frame's
//! last column and row repeated (the guest crops the SPS back). Without them: the type's first encoder at the
//! picture's size. `encctl caps` prints what the phone's encoders say.
//!
//! The codec's input layout: its input format's "stride" and "slice-height" (MediaCodec's, after start) place the
//! frame in the input buffer; a codec that says nothing gets the frame tightly packed.
//!
//! [I] NV12 as COLOR_FormatYUV420SemiPlanar (21) with ByteBuffer input: c2.qti.hevc.encoder accepts it (`encctl hevc`,
//! 2026-11-22). P010 (54) with HEVCProfileMain10 (2): not yet run on the phone. The still-image settings (one keyframe,
//! no reordering, constant-quality) are in `ffi::MediaCodec::open`.
//!
//! The session's logic ([`CodecSession`], [`Layout`]) is over a [`Codec`]: MediaCodec on Android (`ffi`), a fake in the
//! host tests.
use crate::backend::{Coding, Polled, Session};
use std::sync::atomic::{AtomicU8, Ordering};

/// AMediaCodecBufferInfo's flags.
pub const BUFFER_FLAG_KEY_FRAME: u32 = 1;
pub const BUFFER_FLAG_CODEC_CONFIG: u32 = 2;
pub const BUFFER_FLAG_END_OF_STREAM: u32 = 4;
/// dequeueOutputBuffer's answers that are not a buffer.
pub const INFO_TRY_AGAIN_LATER: isize = -1;
pub const INFO_OUTPUT_FORMAT_CHANGED: isize = -2;
pub const INFO_OUTPUT_BUFFERS_CHANGED: isize = -3;
/// MediaCodecInfo.CodecCapabilities' color formats: COLOR_FormatYUV420SemiPlanar (NV12), COLOR_FormatYUVP010.
pub const COLOR_FORMAT_NV12: i32 = 21;
pub const COLOR_FORMAT_P010: i32 = 54;
/// MediaCodecInfo.CodecProfileLevel.HEVCProfileMain10.
pub const HEVC_PROFILE_MAIN10: i32 = 2;

/// Where this process's MediaCodec open is ([`OPEN_STEPS`]; 0: in none): the helper's watchdog names it when an open
/// takes long (MediaCodec's open did not come back in the phone's helper, 2026-11-23).
pub static OPEN_STEP: AtomicU8 = AtomicU8::new(0);
pub const OPEN_STEPS: [&str; 6] = [
    "none",
    "the codec list (AMediaCodecStore_findNextEncoderForFormat)",
    "AMediaCodec_createCodecByName / createEncoderByType",
    "AMediaCodec_configure",
    "AMediaCodec_start",
    "AMediaCodec_getInputFormat",
];

std::thread_local! {
    /// True on the warm-up thread so its background MediaCodec probe does not overwrite the main thread's OPEN_STEP.
    static IGNORE_OPEN_STEP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks the current thread (warm-up) so its MediaCodec calls do not clobber the watchdog's OPEN_STEP.
pub fn ignore_open_step_on_thread() {
    IGNORE_OPEN_STEP.with(|f| f.set(true));
}

pub fn set_open_step(step: u8) {
    if !IGNORE_OPEN_STEP.with(|f| f.get()) {
        OPEN_STEP.store(step, Ordering::Relaxed);
    }
}

/// The name of the step an open is in now.
pub fn open_step() -> &'static str {
    OPEN_STEPS.get(OPEN_STEP.load(Ordering::Relaxed) as usize).copied().unwrap_or("?")
}

/// An output buffer's description (AMediaCodecBufferInfo).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BufferInfo {
    pub offset: i32,
    pub size: i32,
    pub presentation_time_us: i64,
    pub flags: u32,
}

/// What a session needs of a MediaCodec encoder (the AMediaCodec calls backend_ndk.c made).
pub trait Codec: Send {
    /// An input buffer's index, or < 0 (after `timeout_us`).
    fn dequeue_input(&mut self, timeout_us: i64) -> isize;
    fn input_buffer(&mut self, index: usize) -> Option<&mut [u8]>;
    /// Queues `size` bytes of input buffer `index`; whether MediaCodec took it.
    fn queue_input(&mut self, index: usize, size: usize, pts_us: u64, flags: u32) -> bool;
    /// An output buffer's index (its description into `info`), or an INFO_* value or another error (< 0).
    fn dequeue_output(&mut self, info: &mut BufferInfo, timeout_us: i64) -> isize;
    fn output_buffer(&mut self, index: usize) -> Option<&[u8]>;
    fn release_output(&mut self, index: usize);
}

/// How a fed frame goes into the codec's input buffer: the frames' samples and size, the codec's samples, size and
/// rows. Two-plane 4:2:0 both sides (NV12 or P010); every size even.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// bytes a sample of the frames fed and of the codec's input (1: NV12, 2: P010)
    pub input_bytes: u32,
    pub codec_bytes: u32,
    /// the frames' size, and the codec's: at least as large (the frame's last column and row repeated)
    pub width: u32,
    pub height: u32,
    pub coded_width: u32,
    pub coded_height: u32,
    /// the codec's rows: bytes a row, and the luma rows before the chroma plane
    pub stride: u32,
    pub slice_height: u32,
}

impl Layout {
    /// Tightly packed in the codec's buffer.
    pub fn new(
        input_bytes: u32,
        codec_bytes: u32,
        width: u32,
        height: u32,
        coded_width: u32,
        coded_height: u32,
    ) -> Layout {
        Layout {
            input_bytes,
            codec_bytes,
            width,
            height,
            coded_width,
            coded_height,
            stride: coded_width * codec_bytes,
            slice_height: coded_height,
        }
    }

    /// The codec's own rows (its input format's "stride" and "slice-height"), where they hold the picture: a stride
    /// shorter than a row in bytes but not in samples is taken as samples (P010's is said either way).
    pub fn with_rows(mut self, stride: i32, slice_height: i32) -> Layout {
        let row = self.coded_width * self.codec_bytes;
        if let Ok(stride) = u32::try_from(stride) {
            if stride >= row {
                self.stride = stride;
            } else if stride * self.codec_bytes >= row {
                self.stride = stride * self.codec_bytes;
            }
        }
        if let Ok(slice) = u32::try_from(slice_height) {
            if slice >= self.coded_height {
                self.slice_height = slice;
            }
        }
        self
    }

    /// The bytes of a fed frame.
    pub fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 3 / 2 * self.input_bytes as usize
    }

    /// The bytes of the codec's input: the luma plane's rows, then the chroma plane's.
    pub fn buffer_bytes(&self) -> usize {
        self.stride as usize * (self.slice_height as usize + self.coded_height as usize / 2)
    }

    /// A fed frame into the codec's buffer `to`: each row (converted between 8 and 16 bits: an 8-bit sample is v *
    /// 257 in 16, the low 6 bits cleared; a 16-bit sample is its high byte in 8), the last column and row repeated to
    /// the codec's size. False when either is too small or a size is odd.
    pub fn place(&self, frame: &[u8], to: &mut [u8]) -> bool {
        let (w, h) = (self.width as usize, self.height as usize);
        let (cw, ch) = (self.coded_width as usize, self.coded_height as usize);
        let (ib, cb, stride) = (self.input_bytes as usize, self.codec_bytes as usize, self.stride as usize);
        if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 || cw < w || ch < h || cw % 2 != 0 || ch % 2 != 0 {
            return false;
        }
        if !matches!(ib, 1 | 2)
            || !matches!(cb, 1 | 2)
            || frame.len() < self.frame_bytes()
            || to.len() < self.buffer_bytes()
        {
            return false;
        }
        let sample = |row: &[u8], i: usize| -> u32 {
            if ib == 2 {
                u32::from(row[2 * i]) | u32::from(row[2 * i + 1]) << 8
            } else {
                u32::from(row[i]) * 257
            }
        };
        let put = |line: &mut [u8], i: usize, v: u32| {
            if cb == 2 {
                line[2 * i] = (v & 0xc0) as u8;
                line[2 * i + 1] = (v >> 8) as u8;
            } else {
                line[i] = (v >> 8) as u8;
            }
        };
        let copy = |src: &[u8], dst: &mut [u8]| {
            if ib == cb {
                dst[..w * cb].copy_from_slice(&src[..w * ib]);
            } else {
                for x in 0..w {
                    put(dst, x, sample(src, x));
                }
            }
        };
        for y in 0..ch {
            let src = &frame[y.min(h - 1) * w * ib..][..w * ib];
            let dst = &mut to[y * stride..][..cw * cb];
            copy(src, dst);
            let last = sample(src, w - 1);
            for x in w..cw {
                put(dst, x, last);
            }
        }
        let (from, at) = (w * h * ib, stride * self.slice_height as usize);
        for y in 0..ch / 2 {
            let src = &frame[from + y.min(h / 2 - 1) * w * ib..][..w * ib];
            let dst = &mut to[at + y * stride..][..cw * cb];
            copy(src, dst);
            let (u, v) = (sample(src, w - 2), sample(src, w - 1));
            for x in (w..cw).step_by(2) {
                put(dst, x, u);
                put(dst, x + 1, v);
            }
        }
        true
    }
}

/// One encoder session over a codec (`br_enc_session` of backend_ndk.c).
pub struct CodecSession<C: Codec> {
    codec: C,
    layout: Layout,
    coding: Coding,
    eos_queued: bool,
}

impl<C: Codec> CodecSession<C> {
    pub fn new(codec: C, layout: Layout, coding: Coding) -> CodecSession<C> {
        CodecSession { codec, layout, coding, eos_queued: false }
    }
}

impl<C: Codec> Session for CodecSession<C> {
    fn feed(&mut self, frame: &[u8], pts_us: i64, _force_key: bool) -> Result<(), i32> {
        if frame.len() < self.layout.frame_bytes() {
            return Err(libc::EINVAL);
        }
        let ix = self.codec.dequeue_input(10_000);
        if ix < 0 {
            return Err(libc::EAGAIN);
        }
        let ix = ix as usize;
        let size = self.layout.buffer_bytes();
        let layout = self.layout;
        let placed = self.codec.input_buffer(ix).is_some_and(|input| layout.place(frame, input));
        if !placed {
            self.codec.queue_input(ix, 0, 0, 0);
            return Err(libc::EINVAL);
        }
        if self.codec.queue_input(ix, size, pts_us as u64, 0) {
            Ok(())
        } else {
            Err(libc::EIO)
        }
    }

    fn drain(&mut self) -> Result<(), i32> {
        if self.eos_queued {
            return Ok(());
        }
        let ix = self.codec.dequeue_input(10_000);
        if ix < 0 {
            return Err(libc::EAGAIN);
        }
        self.eos_queued = self.codec.queue_input(ix as usize, 0, 0, BUFFER_FLAG_END_OF_STREAM);
        if self.eos_queued {
            Ok(())
        } else {
            Err(libc::EIO)
        }
    }

    fn poll(&mut self, buf: &mut [u8]) -> Result<Polled, i32> {
        let mut info = BufferInfo::default();
        let ix = self.codec.dequeue_output(&mut info, 0);
        if matches!(ix, INFO_TRY_AGAIN_LATER | INFO_OUTPUT_FORMAT_CHANGED | INFO_OUTPUT_BUFFERS_CHANGED) {
            return Ok(Polled::default());
        }
        if ix < 0 {
            return Err(libc::EIO);
        }
        let ix = ix as usize;
        let mut p = Polled::default();
        let mut result = Ok(());
        let size = info.size.max(0) as usize;
        let at = info.offset.max(0) as usize;
        match self.codec.output_buffer(ix) {
            Some(out) if size <= buf.len() && at + size <= out.len() => {
                buf[..size].copy_from_slice(&out[at..at + size]);
                p.bytes = size;
                p.pts_us = info.presentation_time_us;
                p.key = info.flags & (BUFFER_FLAG_KEY_FRAME | BUFFER_FLAG_CODEC_CONFIG) != 0;
            }
            _ if size > buf.len() => result = Err(libc::ENOSPC),
            _ => {}
        }
        p.eos = info.flags & BUFFER_FLAG_END_OF_STREAM != 0;
        self.codec.release_output(ix);
        result.map(|()| p)
    }

    fn coding(&self) -> Coding {
        self.coding
    }
}

/// The size an encoder codes a width x height picture at: at least its smallest (`min`), rounded up to its
/// alignment (`align`, a power of two; 2 at least: 4:2:0).
pub fn coded_size(width: u32, height: u32, min: (u32, u32), align: (u32, u32)) -> (u32, u32) {
    let up = |v: u32, m: u32, a: u32| {
        let a = a.max(2);
        v.max(m).div_ceil(a) * a
    };
    (up(width, min.0, align.0), up(height, min.1, align.1))
}

#[cfg(target_os = "android")]
pub use ffi::{caps, Caps, NdkBackend};

/// AMediaCodec of libmediandk (linked into the programs only: build.rs); Android 16's codec capabilities by dlsym.
#[cfg(target_os = "android")]
mod ffi {
    use super::{
        coded_size, set_open_step, BufferInfo, Codec, CodecSession, Layout, COLOR_FORMAT_NV12, COLOR_FORMAT_P010,
        HEVC_PROFILE_MAIN10,
    };
    use crate::backend::{Backend, Coding, Session};
    use crate::proto::{MEDIA_PIXFMT_HEVC, MEDIA_PIXFMT_P010};
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::sync::{Mutex, OnceLock};

    #[repr(C)]
    struct AMediaCodec {
        _p: [u8; 0],
    }
    #[repr(C)]
    struct AMediaFormat {
        _p: [u8; 0],
    }
    const AMEDIA_OK: i32 = 0;
    const CONFIGURE_FLAG_ENCODE: u32 = 1;
    const BITRATE_MODE_CQ: i32 = 0;
    const BITRATE_MODE_VBR: i32 = 1;

    extern "C" {
        static AMEDIAFORMAT_KEY_MIME: *const c_char;
        static AMEDIAFORMAT_KEY_WIDTH: *const c_char;
        static AMEDIAFORMAT_KEY_HEIGHT: *const c_char;
        static AMEDIAFORMAT_KEY_COLOR_FORMAT: *const c_char;
        static AMEDIAFORMAT_KEY_FRAME_RATE: *const c_char;
        static AMEDIAFORMAT_KEY_BIT_RATE: *const c_char;
        fn AMediaCodec_createEncoderByType(mime: *const c_char) -> *mut AMediaCodec;
        fn AMediaCodec_createCodecByName(name: *const c_char) -> *mut AMediaCodec;
        fn AMediaCodec_configure(
            c: *mut AMediaCodec,
            f: *const AMediaFormat,
            w: *mut c_void,
            crypto: *mut c_void,
            flags: u32,
        ) -> i32;
        fn AMediaCodec_start(c: *mut AMediaCodec) -> i32;
        fn AMediaCodec_stop(c: *mut AMediaCodec) -> i32;
        fn AMediaCodec_delete(c: *mut AMediaCodec) -> i32;
        fn AMediaCodec_getInputFormat(c: *mut AMediaCodec) -> *mut AMediaFormat;
        fn AMediaCodec_dequeueInputBuffer(c: *mut AMediaCodec, timeout_us: i64) -> isize;
        fn AMediaCodec_getInputBuffer(c: *mut AMediaCodec, idx: usize, size: *mut usize) -> *mut u8;
        fn AMediaCodec_queueInputBuffer(
            c: *mut AMediaCodec,
            idx: usize,
            offset: i64,
            size: usize,
            pts: u64,
            flags: u32,
        ) -> i32;
        fn AMediaCodec_dequeueOutputBuffer(c: *mut AMediaCodec, info: *mut BufferInfo, timeout_us: i64) -> isize;
        fn AMediaCodec_getOutputBuffer(c: *mut AMediaCodec, idx: usize, size: *mut usize) -> *mut u8;
        fn AMediaCodec_releaseOutputBuffer(c: *mut AMediaCodec, idx: usize, render: bool) -> i32;
        fn AMediaFormat_new() -> *mut AMediaFormat;
        fn AMediaFormat_delete(f: *mut AMediaFormat) -> i32;
        fn AMediaFormat_setString(f: *mut AMediaFormat, name: *const c_char, value: *const c_char);
        fn AMediaFormat_setInt32(f: *mut AMediaFormat, name: *const c_char, value: i32);
        fn AMediaFormat_getInt32(f: *mut AMediaFormat, name: *const c_char, out: *mut i32) -> bool;
    }

    fn mime(codec: u32) -> &'static CStr {
        if codec == MEDIA_PIXFMT_HEVC {
            c"video/hevc"
        } else {
            c"video/avc"
        }
    }

    /// A new AMediaFormat with the mime, optional size (omitted when either is 0, since
    /// `AMediaCodecStore_findNextEncoderForFormat` matches every key set and rejects 0x0) and,
    /// given, the color format and profile; deleted by the caller.
    ///
    /// # Safety
    /// NDK calls; the caller deletes the format.
    unsafe fn format(
        codec: u32,
        width: u32,
        height: u32,
        color: Option<i32>,
        profile: Option<i32>,
    ) -> *mut AMediaFormat {
        // SAFETY: NDK calls on the format made here, with C strings that live through them.
        unsafe {
            let f = AMediaFormat_new();
            AMediaFormat_setString(f, AMEDIAFORMAT_KEY_MIME, mime(codec).as_ptr());
            if width > 0 && height > 0 {
                AMediaFormat_setInt32(f, AMEDIAFORMAT_KEY_WIDTH, width as i32);
                AMediaFormat_setInt32(f, AMEDIAFORMAT_KEY_HEIGHT, height as i32);
            }
            if let Some(color) = color {
                AMediaFormat_setInt32(f, AMEDIAFORMAT_KEY_COLOR_FORMAT, color);
            }
            if let Some(profile) = profile {
                AMediaFormat_setInt32(f, c"profile".as_ptr(), profile);
            }
            f
        }
    }

    // ---- Android 16's codec capabilities (API 36), by dlsym ----------------------------------------------------
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct AIntRange {
        lower: i32,
        upper: i32,
    }

    type FindNext = unsafe extern "C" fn(*const AMediaFormat, *mut *const c_void) -> i32;
    type InfoName = unsafe extern "C" fn(*const c_void) -> *const c_char;
    type InfoInt = unsafe extern "C" fn(*const c_void) -> i32;
    type FormatSupported = unsafe extern "C" fn(*const c_void, *const AMediaFormat) -> i32;
    type VideoCaps = unsafe extern "C" fn(*const c_void, *mut *const c_void) -> i32;
    type Range = unsafe extern "C" fn(*const c_void, *mut AIntRange) -> i32;
    type SizeSupported = unsafe extern "C" fn(*const c_void, i32, i32) -> i32;

    /// The API 36 functions this file uses (libmediandk of Android 16 has them; an older one has none).
    struct Store {
        find_next_encoder: FindNext,
        canonical_name: InfoName,
        is_vendor: InfoInt,
        is_format_supported: FormatSupported,
        video_caps: VideoCaps,
        widths: Range,
        heights: Range,
        width_alignment: InfoInt,
        height_alignment: InfoInt,
        size_supported: SizeSupported,
    }

    fn store() -> Option<&'static Store> {
        static STORE: OnceLock<Option<Store>> = OnceLock::new();
        STORE
            .get_or_init(|| {
                let sym = |name: &CStr| {
                    // SAFETY: dlsym of a C string in the global namespace (libmediandk is linked into this program).
                    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
                    (!p.is_null()).then_some(p)
                };
                // SAFETY: each pointer is the API 36 function of that name, transmuted to its C signature.
                unsafe {
                    Some(Store {
                        find_next_encoder: std::mem::transmute::<*mut c_void, FindNext>(sym(
                            c"AMediaCodecStore_findNextEncoderForFormat",
                        )?),
                        canonical_name: std::mem::transmute::<*mut c_void, InfoName>(sym(
                            c"AMediaCodecInfo_getCanonicalName",
                        )?),
                        is_vendor: std::mem::transmute::<*mut c_void, InfoInt>(sym(c"AMediaCodecInfo_isVendor")?),
                        is_format_supported: std::mem::transmute::<*mut c_void, FormatSupported>(sym(
                            c"AMediaCodecInfo_isFormatSupported",
                        )?),
                        video_caps: std::mem::transmute::<*mut c_void, VideoCaps>(sym(
                            c"AMediaCodecInfo_getVideoCapabilities",
                        )?),
                        widths: std::mem::transmute::<*mut c_void, Range>(sym(
                            c"ACodecVideoCapabilities_getSupportedWidths",
                        )?),
                        heights: std::mem::transmute::<*mut c_void, Range>(sym(
                            c"ACodecVideoCapabilities_getSupportedHeights",
                        )?),
                        width_alignment: std::mem::transmute::<*mut c_void, InfoInt>(sym(
                            c"ACodecVideoCapabilities_getWidthAlignment",
                        )?),
                        height_alignment: std::mem::transmute::<*mut c_void, InfoInt>(sym(
                            c"ACodecVideoCapabilities_getHeightAlignment",
                        )?),
                        size_supported: std::mem::transmute::<*mut c_void, SizeSupported>(sym(
                            c"ACodecVideoCapabilities_isSizeSupported",
                        )?),
                    })
                }
            })
            .as_ref()
    }

    /// An encoder's capabilities (the framework's objects live for the process).
    pub struct Caps {
        pub name: CString,
        pub vendor: bool,
        /// the sizes it takes (lower, upper) and their alignment
        pub widths: (u32, u32),
        pub heights: (u32, u32),
        pub align: (u32, u32),
        codec: u32,
        info: usize,
        video: usize,
    }

    impl Caps {
        /// Whether it codes width x height (8-bit).
        pub fn size(&self, width: u32, height: u32) -> bool {
            let store = store().expect("caps without a store");
            // SAFETY: the framework's video capabilities of this encoder.
            unsafe { (store.size_supported)(self.video as *const c_void, width as i32, height as i32) == 1 }
        }

        /// Whether it takes HEVC Main10 with P010 input at width x height.
        pub fn main10(&self, width: u32, height: u32) -> bool {
            let store = store().expect("caps without a store");
            // SAFETY: NDK calls on a format made and deleted here and the framework's codec info.
            unsafe {
                let f = format(self.codec, width, height, Some(COLOR_FORMAT_P010), Some(HEVC_PROFILE_MAIN10));
                let ok = (store.is_format_supported)(self.info as *const c_void, f) == 1;
                AMediaFormat_delete(f);
                ok
            }
        }

        /// The size it codes a width x height picture at (its smallest, its alignment).
        pub fn coded_size(&self, width: u32, height: u32) -> (u32, u32) {
            coded_size(width, height, (self.widths.0, self.heights.0), self.align)
        }
    }

    /// The first vendor encoder of `codec` (else the first) and its capabilities, once a process; why not, when
    /// this Android has no codec store (before 16) or no such encoder.
    pub fn caps(codec: u32) -> Result<&'static Caps, &'static str> {
        static HEVC: OnceLock<Result<Caps, &'static str>> = OnceLock::new();
        static AVC: OnceLock<Result<Caps, &'static str>> = OnceLock::new();
        let cell = if codec == MEDIA_PIXFMT_HEVC { &HEVC } else { &AVC };
        cell.get_or_init(|| {
            let store = store().ok_or("this Android's libmediandk has no AMediaCodecStore (API 36)")?;
            set_open_step(1);
            // SAFETY: NDK calls on a format made and deleted here; the infos are the framework's, for the process.
            let found = unsafe {
                let f = format(codec, 0, 0, None, None);
                let (mut info, mut first, mut vendor): (*const c_void, *const c_void, *const c_void) =
                    (std::ptr::null(), std::ptr::null(), std::ptr::null());
                while (store.find_next_encoder)(f, &mut info) == 0 && !info.is_null() {
                    if first.is_null() {
                        first = info;
                    }
                    if vendor.is_null() && (store.is_vendor)(info) == 1 {
                        vendor = info;
                    }
                }
                AMediaFormat_delete(f);
                if vendor.is_null() {
                    first
                } else {
                    vendor
                }
            };
            set_open_step(0);
            if found.is_null() {
                return Err("no encoder of the type");
            }
            // SAFETY: the framework's codec info and its capabilities.
            unsafe {
                let name = (store.canonical_name)(found);
                let name = if name.is_null() { CString::default() } else { CStr::from_ptr(name).to_owned() };
                let mut video = std::ptr::null();
                if (store.video_caps)(found, &mut video) != 0 || video.is_null() {
                    return Err("the encoder has no video capabilities");
                }
                let (mut w, mut h) = (AIntRange::default(), AIntRange::default());
                (store.widths)(video, &mut w);
                (store.heights)(video, &mut h);
                let positive = |v: i32| v.max(0) as u32;
                Ok(Caps {
                    name,
                    vendor: (store.is_vendor)(found) == 1,
                    widths: (positive(w.lower), positive(w.upper)),
                    heights: (positive(h.lower), positive(h.upper)),
                    align: (positive((store.width_alignment)(video)), positive((store.height_alignment)(video))),
                    codec,
                    info: found as usize,
                    video: video as usize,
                })
            }
        })
        .as_ref()
        .map_err(|e| *e)
    }

    /// A started MediaCodec encoder; stopped and deleted on drop.
    struct MediaCodec(*mut AMediaCodec);

    // SAFETY: an AMediaCodec may be used from any thread, one at a time (the session is behind the device's lock or
    // the helper's one thread).
    unsafe impl Send for MediaCodec {}

    impl MediaCodec {
        /// The encoder `name` (else the type's first) of `codec` at width x height with the still-image settings,
        /// P010 and Main10 for `ten`, at constant quality (`cq`) or a bit rate; and its input rows ("stride",
        /// "slice-height"; -1 when it does not say).
        fn open(
            name: Option<&CStr>,
            codec: u32,
            ten: bool,
            cq: bool,
            width: u32,
            height: u32,
        ) -> Option<(MediaCodec, i32, i32)> {
            // SAFETY: NDK calls with valid C strings; every object made here is deleted here or by Drop.
            unsafe {
                set_open_step(2);
                let c = match name {
                    Some(name) => AMediaCodec_createCodecByName(name.as_ptr()),
                    None => AMediaCodec_createEncoderByType(mime(codec).as_ptr()),
                };
                if c.is_null() {
                    set_open_step(0);
                    return None;
                }
                let (color, profile) =
                    if ten { (COLOR_FORMAT_P010, Some(HEVC_PROFILE_MAIN10)) } else { (COLOR_FORMAT_NV12, None) };
                let f = format(codec, width, height, Some(color), profile);
                AMediaFormat_setInt32(f, AMEDIAFORMAT_KEY_FRAME_RATE, 30);
                AMediaFormat_setInt32(f, c"i-frame-interval".as_ptr(), 0); // still: every frame a keyframe
                AMediaFormat_setInt32(f, c"bitrate-mode".as_ptr(), if cq { BITRATE_MODE_CQ } else { BITRATE_MODE_VBR });
                AMediaFormat_setInt32(f, c"quality".as_ptr(), 90);
                AMediaFormat_setInt32(f, AMEDIAFORMAT_KEY_BIT_RATE, (width * height * 4) as i32);
                set_open_step(3);
                let rc = AMediaCodec_configure(c, f, std::ptr::null_mut(), std::ptr::null_mut(), CONFIGURE_FLAG_ENCODE);
                AMediaFormat_delete(f);
                set_open_step(4);
                if rc != AMEDIA_OK || AMediaCodec_start(c) != AMEDIA_OK {
                    AMediaCodec_delete(c);
                    set_open_step(0);
                    return None;
                }
                set_open_step(5);
                let (mut stride, mut slice) = (-1, -1);
                let input = AMediaCodec_getInputFormat(c);
                if !input.is_null() {
                    AMediaFormat_getInt32(input, c"stride".as_ptr(), &mut stride);
                    AMediaFormat_getInt32(input, c"slice-height".as_ptr(), &mut slice);
                    AMediaFormat_delete(input);
                }
                set_open_step(0);
                Some((MediaCodec(c), stride, slice))
            }
        }
    }

    impl Drop for MediaCodec {
        fn drop(&mut self) {
            // SAFETY: a started codec of ours, stopped and deleted once.
            unsafe {
                AMediaCodec_stop(self.0);
                AMediaCodec_delete(self.0);
            }
        }
    }

    impl Codec for MediaCodec {
        fn dequeue_input(&mut self, timeout_us: i64) -> isize {
            // SAFETY: our codec.
            unsafe { AMediaCodec_dequeueInputBuffer(self.0, timeout_us) }
        }

        fn input_buffer(&mut self, index: usize) -> Option<&mut [u8]> {
            let mut cap = 0;
            // SAFETY: a dequeued input buffer of our codec: `cap` bytes, ours until it is queued.
            unsafe {
                let p = AMediaCodec_getInputBuffer(self.0, index, &mut cap);
                (!p.is_null()).then(|| std::slice::from_raw_parts_mut(p, cap))
            }
        }

        fn queue_input(&mut self, index: usize, size: usize, pts_us: u64, flags: u32) -> bool {
            // SAFETY: a dequeued input buffer of our codec.
            unsafe { AMediaCodec_queueInputBuffer(self.0, index, 0, size, pts_us, flags) == AMEDIA_OK }
        }

        fn dequeue_output(&mut self, info: &mut BufferInfo, timeout_us: i64) -> isize {
            // SAFETY: our codec and a BufferInfo of the NDK's layout.
            unsafe { AMediaCodec_dequeueOutputBuffer(self.0, info, timeout_us) }
        }

        fn output_buffer(&mut self, index: usize) -> Option<&[u8]> {
            let mut cap = 0;
            // SAFETY: a dequeued output buffer of our codec: `cap` bytes until it is released.
            unsafe {
                let p = AMediaCodec_getOutputBuffer(self.0, index, &mut cap);
                (!p.is_null()).then(|| std::slice::from_raw_parts(p, cap))
            }
        }

        fn release_output(&mut self, index: usize) {
            // SAFETY: a dequeued output buffer of our codec.
            unsafe { AMediaCodec_releaseOutputBuffer(self.0, index, false) };
        }
    }

    /// The coded sizes MediaCodec refused Main10 at, this process: not tried again.
    fn main10_refused() -> &'static Mutex<Vec<(u32, u32)>> {
        static REFUSED: OnceLock<Mutex<Vec<(u32, u32)>>> = OnceLock::new();
        REFUSED.get_or_init(Mutex::default)
    }

    /// The backend ("mediacodec").
    #[derive(Clone, Copy, Debug, Default)]
    pub struct NdkBackend;

    impl Backend for NdkBackend {
        fn name(&self) -> &'static str {
            "mediacodec"
        }

        fn open(&self, codec: u32, input: u32, width: u32, height: u32) -> Option<Box<dyn Session>> {
            let caps = caps(codec).ok();
            let (cw, ch) = caps.map_or((width, height), |c| c.coded_size(width, height));
            let name = caps.map(|c| c.name.as_c_str()).filter(|n| !n.is_empty());
            let ten = input == MEDIA_PIXFMT_P010;
            let refused =
                |size| main10_refused().lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(&size);
            // encoder-10bit=8bit or nv12 (docs/switches.md): Main10 is never tried
            let main10 = crate::switches::ten_bit() == crate::switches::TenBit::Main10;
            if ten
                && main10
                && codec == MEDIA_PIXFMT_HEVC
                && !refused((cw, ch))
                && caps.is_none_or(|c| c.main10(cw, ch))
            {
                for cq in [true, false] {
                    if let Some((c, stride, slice)) = MediaCodec::open(name, codec, true, cq, cw, ch) {
                        let layout = Layout::new(2, 2, width, height, cw, ch).with_rows(stride, slice);
                        return Some(Box::new(CodecSession::new(c, layout, Coding::Main10)));
                    }
                }
                main10_refused().lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((cw, ch));
            }
            let (c, stride, slice) = MediaCodec::open(name, codec, false, true, cw, ch)?;
            let layout = Layout::new(if ten { 2 } else { 1 }, 1, width, height, cw, ch).with_rows(stride, slice);
            Some(Box::new(CodecSession::new(c, layout, if ten { Coding::TenAsEight } else { Coding::Eight })))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A codec of input buffers of `cap` bytes whose output is what was queued, as it was given.
    #[derive(Default)]
    struct Fake {
        cap: usize,
        inputs: Vec<Vec<u8>>,
        queued: Vec<(usize, usize, u64, u32)>,
        outputs: Vec<(Vec<u8>, BufferInfo)>,
        released: Vec<usize>,
        busy: bool,
        refuse: bool,
    }

    impl Codec for Fake {
        fn dequeue_input(&mut self, _timeout_us: i64) -> isize {
            if self.busy {
                return -1;
            }
            self.inputs.push(vec![0; self.cap]);
            self.inputs.len() as isize - 1
        }
        fn input_buffer(&mut self, index: usize) -> Option<&mut [u8]> {
            self.inputs.get_mut(index).map(|v| v.as_mut_slice())
        }
        fn queue_input(&mut self, index: usize, size: usize, pts_us: u64, flags: u32) -> bool {
            self.queued.push((index, size, pts_us, flags));
            if size > 0 {
                let info = BufferInfo {
                    offset: 2,
                    size: size as i32,
                    presentation_time_us: pts_us as i64,
                    flags: BUFFER_FLAG_KEY_FRAME,
                };
                let mut data = vec![0xee, 0xee];
                data.extend_from_slice(&self.inputs[index][..size]);
                self.outputs.push((data, info));
            }
            if flags & BUFFER_FLAG_END_OF_STREAM != 0 {
                self.outputs.push((Vec::new(), BufferInfo { flags, ..Default::default() }));
            }
            !self.refuse
        }
        fn dequeue_output(&mut self, info: &mut BufferInfo, _timeout_us: i64) -> isize {
            match self.outputs.first() {
                Some((_, i)) => {
                    *info = *i;
                    0
                }
                None => INFO_TRY_AGAIN_LATER,
            }
        }
        fn output_buffer(&mut self, _index: usize) -> Option<&[u8]> {
            self.outputs.first().map(|o| o.0.as_slice())
        }
        fn release_output(&mut self, index: usize) {
            self.released.push(index);
            self.outputs.remove(0);
        }
    }

    /// A 2x2 NV12 frame: Y 1 2 3 4, then one chroma pair 5 6.
    const NV12_2X2: [u8; 6] = [1, 2, 3, 4, 5, 6];

    #[test]
    fn a_frame_and_the_end() {
        let layout = Layout::new(1, 1, 2, 2, 2, 2);
        let mut s = CodecSession::new(Fake { cap: 16, ..Default::default() }, layout, Coding::Eight);
        let mut buf = [0u8; 32];
        assert_eq!(s.poll(&mut buf), Ok(Polled::default())); // try again later
        assert_eq!(s.feed(&NV12_2X2, 42, false), Ok(()));
        assert_eq!(s.poll(&mut buf), Ok(Polled { bytes: 6, key: true, pts_us: 42, eos: false }));
        assert_eq!(&buf[..6], &NV12_2X2); // from the buffer's offset
        assert_eq!(s.drain(), Ok(()));
        assert_eq!(s.drain(), Ok(())); // once
        assert_eq!(s.codec.queued.len(), 2);
        assert_eq!(s.poll(&mut buf), Ok(Polled { eos: true, ..Polled::default() }));
        assert_eq!(s.codec.released, [0, 0]);
        assert_eq!(s.coding(), Coding::Eight);
    }

    #[test]
    fn refusals() {
        let layout = Layout::new(1, 1, 2, 2, 2, 2);
        let mut s = CodecSession::new(Fake { cap: 4, ..Default::default() }, layout, Coding::Eight);
        assert_eq!(s.feed(&NV12_2X2[..5], 0, false), Err(libc::EINVAL)); // shorter than a frame: not even dequeued
        assert!(s.codec.queued.is_empty());
        assert_eq!(s.feed(&NV12_2X2, 0, false), Err(libc::EINVAL)); // larger than the input buffer: given back empty
        assert_eq!(s.codec.queued, [(0, 0, 0, 0)]);
        s.codec.busy = true;
        assert_eq!(s.feed(&NV12_2X2, 0, false), Err(libc::EAGAIN));
        assert_eq!(s.drain(), Err(libc::EAGAIN));
        s.codec.busy = false;
        s.codec.cap = 64;
        assert_eq!(s.feed(&NV12_2X2, 0, false), Ok(()));
        assert_eq!(s.poll(&mut [0u8; 4]), Err(libc::ENOSPC)); // a unit larger than the buffer: released, an error
        assert!(s.codec.outputs.is_empty());
        s.codec.refuse = true;
        assert_eq!(s.feed(&NV12_2X2, 0, false), Err(libc::EIO));
    }

    /// A tightly packed P010 frame of w x h: luma (x, y) = 10-bit (x + 4y) * 9, chroma pair x of row y = (300 + x + y,
    /// 600 + x - y), each in the high 10 bits of 16.
    fn p010(w: usize, h: usize) -> Vec<u8> {
        let mut f = Vec::new();
        let mut put = |v: usize| f.extend_from_slice(&(((v % 1024) as u16) << 6).to_le_bytes());
        for y in 0..h {
            for x in 0..w {
                put((x + 4 * y) * 9);
            }
        }
        for y in 0..h / 2 {
            for x in 0..w / 2 {
                put(300 + x + y);
                put(600 + x - y);
            }
        }
        f
    }

    fn at16(b: &[u8], i: usize) -> u32 {
        u32::from(b[2 * i]) | u32::from(b[2 * i + 1]) << 8
    }

    #[test]
    fn main10_takes_p010_as_it_is_and_pads_to_the_codec() {
        // 84x84 P010 into a codec of 96x96 with a stride of 256 bytes and 112 luma rows (its input format's)
        let layout = Layout::new(2, 2, 84, 84, 96, 96).with_rows(256, 112);
        assert_eq!((layout.stride, layout.slice_height), (256, 112));
        let frame = p010(84, 84);
        assert_eq!(frame.len(), layout.frame_bytes());
        let mut to = vec![0u8; layout.buffer_bytes()];
        assert!(layout.place(&frame, &mut to));
        let luma = |x: usize, y: usize| at16(&to[y * 256..], x);
        let chroma = |x: usize, y: usize, c: usize| at16(&to[256 * 112 + y * 256..], 2 * x + c);
        let want = |v: usize| ((v % 1024) as u32) << 6;
        assert_eq!(luma(0, 0), want(0));
        assert_eq!(luma(83, 83), want((83 + 4 * 83) * 9));
        assert_eq!(luma(95, 10), want((83 + 40) * 9)); // the last column repeated
        assert_eq!(luma(5, 95), want((5 + 4 * 83) * 9)); // the last row repeated
        assert_eq!(chroma(3, 2, 0), want(300 + 5));
        assert_eq!(chroma(41, 41, 1), want(600));
        assert_eq!(chroma(47, 47, 0), want(300 + 41 + 41)); // the last pair, last row, repeated
        assert_eq!(chroma(47, 47, 1), want(600));
    }

    #[test]
    fn ten_bit_into_an_eight_bit_codec_is_the_high_byte() {
        let layout = Layout::new(2, 1, 4, 2, 4, 2);
        let frame = p010(4, 2);
        let mut to = vec![0u8; layout.buffer_bytes()];
        assert_eq!(to.len(), 12);
        assert!(layout.place(&frame, &mut to));
        let high = |i: usize| frame[2 * i + 1];
        assert_eq!(to, (0..12).map(high).collect::<Vec<_>>());
        // and an 8-bit frame into a P010 codec: v * 257, the low 6 bits clear
        let layout = Layout::new(1, 2, 2, 2, 2, 2);
        let mut to = vec![0u8; layout.buffer_bytes()];
        assert!(layout.place(&[0, 255, 128, 1, 7, 9], &mut to));
        assert_eq!(at16(&to, 1), 0xffc0);
        assert_eq!(at16(&to, 2), (128 * 257) & 0xffc0);
        assert_eq!(at16(&to, 5), (9 * 257) & 0xffc0);
    }

    #[test]
    fn rows_and_refused_layouts() {
        // a stride given in samples for P010 is taken as bytes; one too short for the row is not taken; a slice
        // height below the picture is not taken
        assert_eq!(Layout::new(2, 2, 96, 96, 96, 96).with_rows(128, -1).stride, 256);
        assert_eq!(Layout::new(2, 2, 96, 96, 96, 96).with_rows(64, 90), Layout::new(2, 2, 96, 96, 96, 96));
        assert_eq!(Layout::new(1, 1, 96, 96, 96, 96).with_rows(128, 128).buffer_bytes(), 128 * (128 + 48));
        let mut to = vec![0u8; 64];
        assert!(!Layout::new(1, 1, 3, 2, 4, 2).place(&[0; 9], &mut to)); // an odd width
        assert!(!Layout::new(1, 1, 4, 2, 2, 2).place(&[0; 12], &mut to)); // a codec smaller than the frame
        assert!(!Layout::new(1, 1, 4, 4, 4, 4).place(&[0; 23], &mut to)); // a short frame
        assert!(!Layout::new(1, 1, 8, 8, 8, 8).place(&[0; 96], &mut to)); // a short buffer
    }

    #[test]
    fn coded_sizes() {
        assert_eq!(coded_size(84, 84, (96, 96), (2, 2)), (96, 96));
        assert_eq!(coded_size(84, 84, (128, 128), (16, 16)), (128, 128));
        assert_eq!(coded_size(130, 98, (96, 96), (16, 8)), (144, 104));
        assert_eq!(coded_size(84, 84, (0, 0), (0, 0)), (84, 84)); // no capabilities: as it is
    }
}
