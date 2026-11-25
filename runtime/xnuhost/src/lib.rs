// SPDX-License-Identifier: MIT
//! xnuhost: XNU as a process on Android's Linux kernel (docs/booter.md). One module per part of the machine.

/// Context switches and trap entry.
pub mod arch;
/// Disks.
pub mod blk;
/// The settings.
pub mod config;
/// The vCPUs.
pub mod cpu;
/// Pages the guest gave back.
pub mod discard;
/// The scanout and the display helper.
pub mod display;
/// One frame end to end: the 10 s `frame:` line.
pub mod frame_path;
/// The interrupt controller.
pub mod gic;
/// The 3D renderer's front.
pub mod gpu3d;
/// The amdgpu native-context renderer (the Galaxy A56's Xclipse, raw ioctls) behind the 3D front.
pub mod gpu3d_amdgpu;
/// The drm2kgsl renderer (Droid-VM's, linked in) behind the 3D front.
pub mod gpu3d_kgsl;
/// The host-call vector.
pub mod hostcalls;
/// The host's memory in full: the breakdown lines (docs/booter.md 2.2).
pub mod hostmem;
/// The image set.
pub mod imageset;
/// /dev/input into virtio-input.
pub mod input;
/// Scripted touches and keys.
pub mod input_script;
/// Code that runs in the kernel's context.
pub mod kctx;
/// Guest RAM and the I/O aperture.
pub mod mem;
/// The guest's motion device: the sensor hub's samples as virtio-input.
pub mod motion;
/// The pen digitizer.
pub mod pen;
/// The perf: report.
pub mod perf;
/// xnuhost --prop-wait: the OneOS stop loop's wait on Android's properties (docs/booter.md 2.9d)
pub mod propwait;
/// The guest's RAM held fixed.
pub mod ramfix;
/// When the guest is up.
pub mod ready;
/// The return's graceful-shutdown state machine.
pub mod ret;
/// The session, the main loop, the stop.
pub mod session;
/// Trapped system registers.
pub mod sysreg;
/// iOS tasks as processes.
pub mod task;
/// The PL011 console.
pub mod uart;
/// The USB gadget's log line.
pub mod usb;
/// The virtio block device.
pub mod virtio_blk;
/// The virtio GPU device.
pub mod virtio_gpu;
/// The virtio input device.
pub mod virtio_input;
