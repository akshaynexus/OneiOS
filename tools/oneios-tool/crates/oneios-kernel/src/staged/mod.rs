//! The edit lists of the staged sources: for each, the lines of an upstream source that change (the anchor, and what
//! replaces it). Every anchor must be in the source exactly once (gen::replace_once), so a moved pin that changes
//! one of these lines stops the build instead of leaving an edit out.

/// The output latency the speaker device publishes when the boot arguments do not say it (frames at 48 kHz). xnuhost
/// passes the helper's measured latency as oneios-audio-latency= (its probe, runtime/bridges/audio); this default is
/// the MMAP exclusive path's predicted host latency (fix-audio-latency, 2026-11-15: 15 ms [I], not measured on the
/// phone; the speaker hands each period on as it starts, so the guest adds none). Before: 6480 (135 ms, the
/// deep-buffer mixer track's path and the guest's period, docs/hardware.md section 6).
macro_rules! output_latency {
    () => {
        720
    };
}
/// NativeHidInput's edge band, as a share of each axis: 3 mm of the iPhone18,2's panel (54 of 1320 x 2868 pixels at
/// 460 per inch). The S24 Ultra's panel is the same size within 1 % (1440 x 3120 at 505 per inch: 60 pixels), so the
/// share holds for the guest's native mode too.
macro_rules! edge_pixels {
    () => {
        54
    };
}
macro_rules! panel_x {
    () => {
        1320
    };
}
macro_rules! panel_y {
    () => {
        2868
    };
}
pub const OUTPUT_LATENCY: u32 = output_latency!();

pub mod audio;
pub mod codec;
pub mod codec_decode;
pub mod codec_transport;
pub mod display;
pub mod display_header;
pub mod gpu;
pub mod hid;
pub mod hid_motion;
pub mod hid_pen;
pub mod hid_transport;
pub mod net_transport;
