// SPDX-License-Identifier: MIT
//! The C ABI of NativeMetal's bridge (NativeMetalBridge.h, NativeMetalObjects.h at b0d6a07), unchanged, over metal2vulkan-rt.
//! Every handle is a box the matching release call frees; a call that fails returns 0/NULL/false and leaves its reason in
//! nmb_last_error() (per thread); no call unwinds into C.

pub mod command;
pub mod device;
pub mod memory;
pub mod shader;
pub mod texture;

use metal2vulkan_runtime as rt;
use std::cell::RefCell;
use std::ffi::c_char;

thread_local! {
    static LAST: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn set_error(why: String) {
    LAST.with(|last| {
        let mut last = last.borrow_mut();
        last.clear();
        last.extend(why.bytes().filter(|b| *b != 0));
        last.push(0);
    });
}

/// Runs `body`, turning an error or a panic into the per-thread reason and `fallback`.
pub(crate) fn guard<T>(fallback: T, body: impl FnOnce() -> Result<T, String>) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(Ok(value)) => value,
        Ok(Err(why)) => {
            set_error(why);
            fallback
        }
        Err(_) => {
            set_error("panic in the Metal runtime".into());
            fallback
        }
    }
}

pub(crate) fn boxed<T>(value: T) -> *mut T {
    Box::into_raw(Box::new(value))
}

/// The object a handle points at.
pub(crate) fn get<'a, T>(handle: *const T) -> Result<&'a T, String> {
    // SAFETY: handles come from `boxed` and live until their release call (the header's contract).
    unsafe { handle.as_ref() }.ok_or_else(|| "null handle".to_string())
}

pub(crate) fn get_mut<'a, T>(handle: *mut T) -> Result<&'a mut T, String> {
    // SAFETY: as in `get`; an encoder is used by one thread at a time (Metal's rule).
    unsafe { handle.as_mut() }.ok_or_else(|| "null handle".to_string())
}

pub(crate) fn release<T>(handle: *mut T) {
    if !handle.is_null() {
        // SAFETY: from `boxed`, released once.
        drop(unsafe { Box::from_raw(handle) });
    }
}

pub(crate) fn copy_out(text: &str, out: *mut c_char, size: usize) -> usize {
    if !out.is_null() && size > 0 {
        let n = text.len().min(size - 1);
        // SAFETY: the caller's buffer holds `size` bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr(), out as *mut u8, n);
            *out.add(n) = 0;
        }
    }
    text.len()
}

/// A raw Metal value through the translator's checked conversion; a value it does not know fails the call.
pub(crate) fn raw<T: TryFrom<u32>>(value: u64, what: &str) -> Result<T, String> {
    u32::try_from(value).ok().and_then(|v| T::try_from(v).ok()).ok_or_else(|| format!("{what} {value} is not a Metal value"))
}

/// The format the runtime holds a texture or a color attachment of Metal format `pf` in. rt makes no texture of Apple's
/// extended-range and padded 10-bit forms (`Gap::TextureFormat`) and blends into no attachment of them (the extended-range
/// ones `Format`, the padded ones `FramebufferFetch`: not implemented on any device). Core Animation renders and blends
/// into them: its blur's `downsample_blur_vert_lph` + `alpha_threshold_lph` over `BGR10_XR` aborted backboardd at every boot
/// on v0.1.36 (issue #81). Each is held in a format of the same bytes a pixel that Turnip renders and blends: the XR
/// forms in floats (values past 1 kept; `RG11B10Float` drops negatives), the padded ones in 16-bit unorm or plain
/// `RGB10A2Unorm` (the codes' sRGB transfer dropped: the values are kept, not the bytes). Every producer and consumer of
/// such a texture must be this runtime, so the device answers `supportsExtendedXR10Formats` false to every process
/// (`device.rs`, 2026-11-28; until then the frontend withheld it from the render server only, and the poster extensions
/// wrote XR surfaces in Apple's bytes that backboardd read through these formats). `nmb_texture_describe` answers the
/// format asked for.
pub fn stored(pf: rt::PixelFormat) -> rt::PixelFormat {
    use rt::PixelFormat as P;
    match pf {
        P::BGR10XR | P::BGR10XRSRGB => P::RG11B10Float,
        P::BGRA10XR | P::BGRA10XRSRGB => P::RGBA16Float,
        P::RGB10A82PXR10 | P::RGB10A82PXR10SRGB => P::RGBA16Float,
        P::RGB10A2UnormSRGB => P::RGB10A2Unorm,
        P::R10UnormX6 | P::R10UnormX6SRGB => P::R16Unorm,
        P::RG10UnormX12 | P::RG10UnormX12SRGB => P::RG16Unorm,
        P::R16Bfloat => P::R16Float,
        P::RG16Bfloat => P::RG16Float,
        P::RGBA16Bfloat => P::RGBA16Float,
        other => other,
    }
}

/// A raw Metal pixel format as the runtime holds it ([`stored`]).
pub(crate) fn pixel_format(value: u64, what: &str) -> Result<rt::PixelFormat, String> {
    raw::<rt::PixelFormat>(value, what).map(stored)
}

pub(crate) fn e<E: std::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> String {
    move |error| format!("{what}: {error}")
}

pub(crate) fn words<'a, T>(p: *const T, n: usize, what: &str) -> Result<&'a [T], String> {
    if p.is_null() {
        return Err(format!("null {what}"));
    }
    // SAFETY: the header's `n` words.
    Ok(unsafe { std::slice::from_raw_parts(p, n) })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_last_error() -> *const c_char {
    LAST.with(|last| {
        let last = last.borrow();
        if last.is_empty() { c"".as_ptr() } else { last.as_ptr() as *const c_char }
    })
}

/// Metal's resource options word: the storage mode (bits 4..8), the CPU cache mode (0..4), the hazard tracking (8..10).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Options(pub u64);

impl Options {
    pub fn storage(self) -> u64 {
        (self.0 >> 4) & 0xf
    }
    pub fn is_private(self) -> bool {
        self.storage() == 2
    }
    pub fn class(self) -> metal2vulkan_runtime::MemoryClass {
        if self.is_private() { metal2vulkan_runtime::MemoryClass::Private } else { metal2vulkan_runtime::MemoryClass::Shared }
    }
    pub fn storage_mode(self) -> metal2vulkan_runtime::StorageMode {
        use metal2vulkan_runtime::StorageMode;
        match self.storage() {
            1 => StorageMode::Managed,
            // Memoryless is backed by a Private texture: rt builds no Memoryless storage (Gap::MemorylessTexture), and Core
            // Animation's attachments are memoryless.
            2 | 3 => StorageMode::Private,
            _ => StorageMode::Shared,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::stored;
    use metal2vulkan_runtime::PixelFormat as P;

    /// Each of Apple's forms rt makes no texture of is held in a format of the same bytes a pixel; every other format is
    /// held as it is.
    #[test]
    fn apple_forms_are_held_in_formats_of_their_size() {
        let size = |p: P| match p {
            P::BGR10XR | P::BGR10XRSRGB | P::RGB10A2UnormSRGB | P::RG10UnormX12 | P::RG10UnormX12SRGB | P::RG16Bfloat => 4,
            P::BGRA10XR | P::BGRA10XRSRGB | P::RGB10A82PXR10 | P::RGB10A82PXR10SRGB | P::RGBA16Bfloat => 8,
            P::R10UnormX6 | P::R10UnormX6SRGB | P::R16Bfloat => 2,
            P::RG11B10Float | P::RGB10A2Unorm | P::RG16Unorm | P::RG16Float => 4,
            P::RGBA16Float => 8,
            P::R16Unorm | P::R16Float => 2,
            _ => 0,
        };
        for p in [
            P::BGR10XR, P::BGR10XRSRGB, P::BGRA10XR, P::BGRA10XRSRGB, P::RGB10A82PXR10, P::RGB10A82PXR10SRGB,
            P::RGB10A2UnormSRGB, P::R10UnormX6, P::R10UnormX6SRGB, P::RG10UnormX12, P::RG10UnormX12SRGB,
            P::R16Bfloat, P::RG16Bfloat, P::RGBA16Bfloat,
        ] {
            assert_ne!(stored(p), p, "{p:?}");
            assert_eq!(size(stored(p)), size(p), "{p:?}");
        }
        for p in [P::BGRA8Unorm, P::BGRA8UnormSRGB, P::RGBA16Float, P::A8Unorm, P::R8Unorm, P::Depth32Float] {
            assert_eq!(stored(p), p);
        }
    }
}
