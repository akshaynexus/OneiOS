// SPDX-License-Identifier: MIT
//! The native Metal bridge on metal2vulkan-rt (metal2vulkan `development`, the runtime rewritten as one path for every
//! device): the C ABI of NativeMetal's bridge (NativeMetalBridge.h, NativeMetalObjects.h at b0d6a07), kept the same for the
//! plug-in and the guest's Metal frontend, over the new runtime. userland/metal/rt/README.md maps the old bridge's calls to
//! this one. rt is GPL-2.0-or-later OR LGPL-3.0-or-later and is linked from its own source (with userland/metal/rt/patches);
//! nothing of it is copied here.

pub mod abi;
pub mod census;
pub mod driver;
pub mod indices;
pub mod selftest;
