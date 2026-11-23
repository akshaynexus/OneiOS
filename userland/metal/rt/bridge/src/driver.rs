// SPDX-License-Identifier: MIT
//! The Vulkan driver opened with no loader: iOS has no Khronos loader, so the bridge loads Turnip's
//! dylib itself and hands its instance entry to metal2vulkan-rt (`Instance::from_driver`, rt patch
//! 0001). The same path opens ANV or lavapipe on Linux for the host tests (`m2v-rt-check`).

use ash::vk;
use metal2vulkan_runtime as rt;
use std::ffi::{c_char, c_void, CStr, CString};

/// A driver's library, loaded for the life of the process (never closed: the instance and every
/// object made from it call into it), and its instance entry.
pub struct Driver {
    pub entry: vk::PFN_vkGetInstanceProcAddr,
    /// Which symbol the entry is: `vk_icdGetInstanceProcAddr` (a driver built for the loader) or
    /// `vkGetInstanceProcAddr`.
    pub entry_name: &'static str,
}

/// Loads the driver at `path` and finds its instance entry.
pub fn open(path: &str) -> Result<Driver, String> {
    let c = CString::new(path).map_err(|_| "a path with a NUL".to_string())?;
    // SAFETY: a library path; loading a Vulkan driver runs its initialisers and nothing else.
    let handle = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        // SAFETY: dlerror's text is valid until the next dl call on this thread.
        let why = unsafe { CStr::from_ptr(libc::dlerror()) }.to_string_lossy().into_owned();
        return Err(format!("dlopen {path}: {why}"));
    }
    for name in ["vk_icdGetInstanceProcAddr", "vkGetInstanceProcAddr"] {
        let symbol = CString::new(name).unwrap();
        // SAFETY: the handle is live.
        let found = unsafe { libc::dlsym(handle, symbol.as_ptr()) };
        if !found.is_null() {
            // The interface version a loader would negotiate first: Mesa answers 7 and keeps it.
            let negotiate = CString::new("vk_icdNegotiateLoaderICDInterfaceVersion").unwrap();
            // SAFETY: the handle is live.
            let n = unsafe { libc::dlsym(handle, negotiate.as_ptr()) };
            if !n.is_null() {
                let mut version: u32 = 7;
                // SAFETY: the driver's own export of this signature.
                let negotiate: unsafe extern "system" fn(*mut u32) -> vk::Result = unsafe { std::mem::transmute(n) };
                let _ = unsafe { negotiate(&mut version) };
            }
            // SAFETY: the symbol is the driver's vkGetInstanceProcAddr-shaped entry.
            let entry: vk::PFN_vkGetInstanceProcAddr = unsafe {
                std::mem::transmute::<*mut c_void, unsafe extern "system" fn(vk::Instance, *const c_char) -> vk::PFN_vkVoidFunction>(found)
            };
            return Ok(Driver { entry, entry_name: if name.starts_with("vk_icd") { "vk_icdGetInstanceProcAddr" } else { "vkGetInstanceProcAddr" } });
        }
    }
    Err(format!("{path} exports neither vk_icdGetInstanceProcAddr nor vkGetInstanceProcAddr"))
}

impl Driver {
    /// The runtime's instance over this driver alone.
    pub fn instance(&self) -> Result<rt::Instance, rt::Error> {
        // SAFETY: the driver's library is never closed.
        unsafe { rt::Instance::from_driver(self.entry, &[]) }
    }
}

/// The device's name, as the driver answers it.
pub fn device_name(instance: &rt::Instance, physical: &rt::PhysicalDevice) -> String {
    let (_, raw) = instance.raw();
    // SAFETY: the instance listed the device.
    let properties = unsafe { raw.get_physical_device_properties(physical.raw()) };
    properties.device_name_as_c_str().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}
