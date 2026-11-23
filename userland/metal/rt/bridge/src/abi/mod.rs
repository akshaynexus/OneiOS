// SPDX-License-Identifier: MIT
//! The C ABI of NativeMetal's bridge (NativeMetalBridge.h, NativeMetalObjects.h at b0d6a07), unchanged, over metal2vulkan-rt.
//! Every handle is a box the matching release call frees; a call that fails returns 0/NULL/false and leaves its reason in
//! nmb_last_error() (per thread); no call unwinds into C.

pub mod command;
pub mod device;
pub mod memory;
pub mod shader;
pub mod texture;

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
