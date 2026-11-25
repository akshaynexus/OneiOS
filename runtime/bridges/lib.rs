// SPDX-License-Identifier: MIT
//! OneiOS's host bridges (docs/hardware.md): the Linux/Android side of each device the guest's drivers use.

/// Sound.
pub mod audio;
/// The battery.
pub mod battery;
/// The backlight and the light sensor.
pub mod brightness;
/// Bluetooth.
pub mod bt;
/// Host topology, the vCPU plan, thread placement.
pub mod cpu;
/// The virtio device models on xnuhost's transport.
pub mod devices;
/// The treble HAL clients.
pub mod hal;
/// The haptic actuator.
pub mod haptics;
/// The hardware backends and the host device.
pub mod hw;
/// The bridges' log (br_log): every bridge's lines, a sink xnuhost sets.
pub mod log;
/// The network bridge.
pub mod net;
/// The protocols shared with the guest's drivers (drivers/bridge, drivers/net, drivers/haptics).
pub mod proto;
/// The motion sensors and the front light sensor: the sensor hub (SSC) over QRTR.
pub mod sensors;
/// The switches xnuhost sets once from its config (docs/switches.md).
pub mod switches;
/// Temperatures and thermal steps.
pub mod thermal;
/// Wi-Fi.
pub mod wifi;
