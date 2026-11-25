// SPDX-License-Identifier: MIT
//! The virtio-input device (VirtIO 1.2, 5.8): the multitouch touchscreen with the phone's side keys, or (a second
//! device, pen.rs) the pen. virtio-input carries Linux evdev events unchanged, which is what the phone's /dev/input
//! gives (input.rs), and what UniXNU-droidvm's DroidVMVirtioInput.c turns into IOHID digitizer reports.
use std::cell::RefCell;
use std::sync::Arc;

use xh_core::logi;
use xh_core::virtio::{self, Viov, Virtio};

use crate::input::ev::{EV_KEY, EV_SYN, KEY_POWER, KEY_VOLUMEDOWN, KEY_VOLUMEUP};
use crate::input::{clip, EventSink};

const CFG_ID_NAME: u8 = 1;
const CFG_ID_SERIAL: u8 = 2;
const CFG_ID_DEVIDS: u8 = 3;
const CFG_PROP_BITS: u8 = 0x10;
const CFG_EV_BITS: u8 = 0x11;
const CFG_ABS_INFO: u8 = 0x12;
const ABS_COUNT: usize = 0x40;
const BACKLOG: usize = 1024;
const MAX_KEYS: usize = 8;

/// The touch screen's keys (the default): the side keys and BTN_TOUCH. The pen's device (pen.rs) states its own.
const TOUCH_KEYS: [u16; 4] =
    [114 /* KEY_VOLUMEDOWN */, 115 /* KEY_VOLUMEUP */, 116 /* KEY_POWER */, 0x14a /* BTN_TOUCH */];

/// The guest's virtio-input device, as the board holds it.
pub type InputDev = Arc<Virtio<Input>>;

/// One event as the guest reads it (struct virtio_input_event).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Event {
    kind: u16,
    code: u16,
    value: i32,
}

impl Event {
    fn bytes(self) -> [u8; 8] {
        let mut b = [0u8; 8];
        b[0..2].copy_from_slice(&self.kind.to_le_bytes());
        b[2..4].copy_from_slice(&self.code.to_le_bytes());
        b[4..8].copy_from_slice(&self.value.to_le_bytes());
        b
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Abs {
    min: i32,
    max: i32,
    fuzz: i32,
    flat: i32,
    res: i32,
    present: bool,
}

struct State {
    select: u8,
    subsel: u8,
    abs: [Abs; ABS_COUNT],
    backlog: Box<[Event; BACKLOG]>, // events the guest had no buffer for
    head: usize,
    count: usize,
    lost: u64,      // events dropped because the backlog was full (the guest posted no buffers)
    losing: bool,   // in such an episode: told once, and when it ends
    name: String,   // the configuration's ID_NAME (at most 31 bytes)
    serial: String, // ID_SERIAL
    keys: Vec<u16>, // its EV_KEY bits (at most 8)
    props: u8,      // its INPUT_PROP_* bits (the touch screen's and the pen's: INPUT_PROP_DIRECT)
}

/// The device's state (behind the device's lock).
pub struct Input {
    st: RefCell<State>,
}

impl State {
    /// The configuration space answer for (select, subsel): its size and bytes.
    fn payload(&self, out: &mut [u8; 128]) -> u8 {
        let copy = |out: &mut [u8; 128], b: &[u8]| {
            out[..b.len()].copy_from_slice(b);
            b.len() as u8
        };
        match self.select {
            CFG_ID_NAME => copy(out, self.name.as_bytes()),
            CFG_ID_SERIAL => copy(out, self.serial.as_bytes()),
            CFG_ID_DEVIDS => {
                // BUS_VIRTUAL, vendor, product, version
                let ids: [u16; 4] = [0x06, 0x4f69, 0x0001, 1];
                for (i, v) in ids.iter().enumerate() {
                    out[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
                }
                8
            }
            CFG_PROP_BITS => {
                out[0] = self.props;
                1
            }
            CFG_EV_BITS if self.subsel == EV_KEY as u8 => {
                let mut top = 0;
                for &k in &self.keys {
                    out[k as usize / 8] |= 1 << (k % 8);
                    top = top.max(k as usize / 8 + 1);
                }
                top as u8
            }
            CFG_EV_BITS if self.subsel == crate::input::ev::EV_ABS as u8 => {
                let mut top = 0;
                for a in (0..ABS_COUNT).filter(|&a| self.abs[a].present) {
                    out[a / 8] |= 1 << (a % 8);
                    top = a / 8 + 1;
                }
                top as u8
            }
            CFG_ABS_INFO if (self.subsel as usize) < ABS_COUNT && self.abs[self.subsel as usize].present => {
                let a = &self.abs[self.subsel as usize];
                for (i, v) in [a.min, a.max, a.fuzz, a.flat, a.res].iter().enumerate() {
                    out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                20
            }
            _ => 0,
        }
    }
}

impl Input {
    /// Moves backlog events into buffers the guest has posted on the event queue.
    fn drain(&self, dev: &Virtio<Input>, s: &mut State) {
        let mut iov = [Viov::EMPTY; 4];
        while s.count > 0 {
            let Some((head, _)) = dev.pop(0, &mut iov) else { return };
            if iov[0].writable() && iov[0].len() >= 8 {
                iov[0].write_at(0, &s.backlog[s.head].bytes());
                s.head = (s.head + 1) % BACKLOG;
                s.count -= 1;
                dev.push(0, head, 8);
            } else {
                dev.push(0, head, 0);
            }
        }
        if s.losing {
            // all handed over: the episode ends
            s.losing = false;
            logi!("input: {}: the guest takes events again ({} lost since the boot)", s.name, s.lost);
        }
    }
}

impl virtio::Device for Input {
    fn handle(&self, _dev: &Virtio<Self>, _queue: u32, _head: u16, _iov: &[Viov]) -> Option<u32> {
        Some(0) // queue 1 carries status (LEDs), which a touchscreen ignores
    }

    fn config_read(&self, _dev: &Virtio<Self>, offset: u32, size: u32) -> u32 {
        let s = self.st.borrow();
        let mut config = [0u8; 136];
        config[0] = s.select;
        config[1] = s.subsel;
        let mut payload = [0u8; 128];
        config[2] = s.payload(&mut payload);
        config[8..].copy_from_slice(&payload);
        let (offset, size) = (offset as usize, size as usize);
        let mut v = [0u8; 4];
        if offset < config.len() && size <= config.len() - offset {
            let n = size.min(4);
            v[..n].copy_from_slice(&config[offset..offset + n]);
        }
        u32::from_le_bytes(v)
    }

    fn config_write(&self, _dev: &Virtio<Self>, offset: u32, _size: u32, value: u32) {
        let mut s = self.st.borrow_mut();
        match offset {
            0 => s.select = value as u8,
            1 => s.subsel = value as u8,
            _ => {}
        }
    }

    /// The guest posted event buffers: hand over what was waiting for them (at a report boundary).
    fn kick(&self, dev: &Virtio<Self>, _queue: u32) {
        let mut s = self.st.borrow_mut();
        if s.count > 0 && s.backlog[(s.head + s.count - 1) % BACKLOG].kind == EV_SYN {
            self.drain(dev, &mut s);
        }
    }
}

/// The virtio-input device at `base` (0: not on the MMIO bus): "OneiOS touchscreen" with the side keys.
pub fn create(base: u64, spi: u32) -> Option<InputDev> {
    let st = State {
        select: 0,
        subsel: 0,
        abs: [Abs::default(); ABS_COUNT],
        backlog: Box::new([Event::default(); BACKLOG]),
        head: 0,
        count: 0,
        lost: 0,
        losing: false,
        name: String::new(),
        serial: String::new(),
        keys: Vec::new(),
        props: 1 << 1, // INPUT_PROP_DIRECT
    };
    let info = virtio::Info { device_id: 18, features: 0, nqueues: 2, manual: 1 };
    let dev = Virtio::create(base, spi, info, Input { st: RefCell::new(st) }, "virtio-input").ok()?;
    set_identity(&dev, "OneiOS touchscreen", "xnuhost-0", &TOUCH_KEYS);
    Some(dev)
}

/// The device's name, serial and key codes (at most 8 keys, each under 512: 64 bytes of key bits in the answer).
pub fn set_identity(dev: &Virtio<Input>, name: &str, serial: &str, keys: &[u16]) {
    let g = dev.lock();
    let mut s = g.st.borrow_mut();
    s.name = clip(name, 31);
    s.serial = clip(serial, 31);
    s.keys = keys.iter().take(MAX_KEYS).map(|&k| if k < 512 { k } else { 0 }).collect();
}

/// The device's INPUT_PROP_* bits (default INPUT_PROP_DIRECT; the motion device: INPUT_PROP_ACCELEROMETER, motion.rs).
pub fn set_props(dev: &Virtio<Input>, props: u8) {
    let g = dev.lock();
    g.st.borrow_mut().props = props;
}

/// An axis of the device's configuration (what input.rs takes from EVIOCGABS).
pub fn set_abs(dev: &Virtio<Input>, axis: u32, min: i32, max: i32, res: i32) {
    if axis as usize >= ABS_COUNT {
        return;
    }
    let g = dev.lock();
    let mut s = g.st.borrow_mut();
    s.abs[axis as usize] = Abs { min, max, fuzz: 0, flat: 0, res, present: true };
}

/// One event for the guest; events are handed over at a report boundary, so the guest never sees half a touch frame.
pub fn event(dev: &Virtio<Input>, kind: u16, code: u16, value: i32) {
    let g = dev.lock();
    let mut s = g.st.borrow_mut();
    if s.count == BACKLOG {
        // oldest lost
        s.head = (s.head + 1) % BACKLOG;
        s.count -= 1;
        s.lost += 1;
        if !s.losing {
            logi!(
                "input: {}: the guest posted no event buffers for {} events: the oldest are dropped until it does",
                s.name,
                BACKLOG
            );
        }
        s.losing = true;
    }
    // A side key (once per press and release; not the autorepeat): the start of the input path, host to guest.
    if kind == EV_KEY && value != 2 && matches!(code, KEY_VOLUMEDOWN | KEY_VOLUMEUP | KEY_POWER) {
        let key = match code {
            KEY_POWER => "power key",
            KEY_VOLUMEUP => "volume up",
            _ => "volume down",
        };
        logi!(
            "input: {} {}: to the guest ({} event(s) waiting for its buffers)",
            key,
            if value != 0 { "down" } else { "up" },
            s.count
        );
    }
    let at = (s.head + s.count) % BACKLOG;
    s.backlog[at] = Event { kind, code, value };
    s.count += 1;
    if kind == EV_SYN {
        g.drain(dev, &mut s);
    }
}

/// Events still waiting for the guest's buffers.
pub fn backlog(dev: &Virtio<Input>) -> u32 {
    let g = dev.lock();
    let n = g.st.borrow().count;
    n as u32
}

impl EventSink for Virtio<Input> {
    fn event(&self, kind: u16, code: u16, value: i32) {
        event(self, kind, code, value)
    }

    fn backlog(&self) -> u32 {
        backlog(self)
    }
}

#[cfg(test)]
mod tests;
