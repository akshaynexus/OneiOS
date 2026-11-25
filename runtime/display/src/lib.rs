// SPDX-License-Identifier: MIT
//! The display helper (README.md): iOS's frames from xnuhost's session/display.sock on the phone's panel.
//!
//! - [`xhd`] (xhd.c, xhd.h): the display.sock protocol both ends use (this helper and xnuhost's display device), the
//!   letterbox fit, the row copy, direct frames, the snapshot and the layer's buffers. No Android in it.
//! - [`frame_perf`] (frame_perf.c): bounded frame timing statistics.
//! - `jni` (jni.c, feature `jni`, Android only): liboneios-display.so, the native half of OneiOSDisplay.java.
//! - the programs: `oneios-display` (launcher.c, src/bin/launcher.rs) and `displayctl` (displayctl.c).
pub mod drm_probe;
pub mod frame_perf;
pub use xh_core::pixels; // the pixel formats and conversions (include/xnuhost/pixels.h), shared with xnuhost
pub mod sys;
pub mod xhd;

#[cfg(all(feature = "jni", target_os = "android"))]
mod jni;
