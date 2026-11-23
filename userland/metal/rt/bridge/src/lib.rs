// SPDX-License-Identifier: MIT
//! The native Metal bridge on metal2vulkan-rt (metal2vulkan `development`, the runtime rewritten
//! as one path for every device): the C ABI of NativeMetal's bridge (NativeMetalBridge.h,
//! NativeMetalObjects.h), kept the same for the plug-in and the guest's Metal frontend, over the
//! new runtime. userland/metal/rt/README.md maps the old bridge's calls to this one.

pub mod census;
pub mod driver;
pub mod indices;
