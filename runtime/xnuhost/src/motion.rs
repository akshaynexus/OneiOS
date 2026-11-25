// SPDX-License-Identifier: MIT
//! The guest's motion device (sensors=auto|ssc, docs/booter.md "Sensors"): a virtio-input device "OneiOS motion
//! sensors" with INPUT_PROP_ACCELEROMETER, the device UniXNU-droidvm's DroidVMVirtioInput.c takes as motion sensors
//! (crosvm's `--input motion` on the iPadOS 27 board). The guest publishes it as an iPhone's SPU accelerometer,
//! gyroscope, device orientation and compass (kernel/patches/oneios-hosted/hid, oneios-motion=MASK).
//!
//! Axes: ABS_X/Y/Z the gravity vector, 256 per g; ABS_RX/RY/RZ the angular velocity, 16 per degree per second;
//! ABS_HAT0X/HAT0Y/HAT1X the magnetic field, 16 per microtesla (only with a magnetometer). The samples come from the
//! sensor hub (onebridges::sensors), one frame per accelerometer sample.

use onebridges::sensors::{self, Motion};
use xh_core::virtio::Virtio;

use crate::virtio_input::{self, Input, InputDev};

const EV_SYN: u16 = 0;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const INPUT_PROP_ACCELEROMETER: u8 = 1 << 6;
/// ABS_X..ABS_RZ, then ABS_HAT0X, ABS_HAT0Y, ABS_HAT1X: the order of sensors::axes().
pub const CODES: [u16; 9] = [0, 1, 2, 3, 4, 5, 0x10, 0x11, 0x12];
/// A frame is 10 events (9 axes, SYN_REPORT): more than this many waiting for the guest's buffers and the new frame is
/// dropped (counted on the hub's line), so the guest never gets a backlog of stale motion. 6 frames: 120 ms at 50 Hz.
pub const MAX_WAITING: u32 = 60;

/// Names the device and states its axes (before the guest reads its configuration).
pub fn guest_device(dev: &InputDev, mag: bool) {
    virtio_input::set_identity(dev, "OneiOS motion sensors", "xnuhost-motion", &[]);
    virtio_input::set_props(dev, INPUT_PROP_ACCELEROMETER);
    let (a, g, m) = (sensors::ACCEL_MAX, sensors::GYRO_MAX, sensors::MAG_MAX);
    // the resolutions as the kernel states them: units per g (none for g), per degree per second, per microtesla
    for (i, &code) in CODES.iter().enumerate() {
        let (max, res) = match i {
            0..=2 => (a, sensors::PER_G as i32),
            3..=5 => (g, sensors::PER_DEG_S as i32),
            _ if mag => (m, sensors::PER_UT as i32),
            _ => continue,
        };
        virtio_input::set_abs(dev, u32::from(code), -max, max, res);
    }
}

/// One frame to the guest; false (nothing sent) when the guest is behind.
pub fn send(dev: &Virtio<Input>, m: &Motion) -> bool {
    if virtio_input::backlog(dev) > MAX_WAITING {
        return false;
    }
    let v = sensors::axes(m);
    let n = if m.have_mag { 9 } else { 6 };
    for (&code, &value) in CODES.iter().zip(v.iter()).take(n) {
        virtio_input::event(dev, EV_ABS, code, value);
    }
    virtio_input::event(dev, EV_SYN, SYN_REPORT, 0);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_device_is_a_motion_device_and_frames_carry_the_axes() {
        let dev = virtio_input::create(0, 0).unwrap();
        guest_device(&dev, true);
        // its frames reach the backlog whole (no guest buffers here): 10 events each, until the guest is too far behind
        let m = Motion {
            accel: [0.0, 0.0, 9.80665],
            gyro: [0.0, 0.0, 0.0],
            mag: [1.0, 2.0, 3.0],
            have_gyro: true,
            have_mag: true,
        };
        let mut sent = 0;
        while send(&dev, &m) {
            sent += 1;
            assert!(sent < 100);
        }
        assert_eq!(sent, 7); // 0, 10, .., 60 waiting: the eighth is dropped
        assert_eq!(virtio_input::backlog(&dev), 70);
        // without a magnetometer a frame is 7 events
        let dev = virtio_input::create(0, 0).unwrap();
        guest_device(&dev, false);
        assert!(send(&dev, &Motion { have_mag: false, ..m }));
        assert_eq!(virtio_input::backlog(&dev), 7);
    }
}
