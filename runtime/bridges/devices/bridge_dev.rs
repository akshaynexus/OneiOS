// SPDX-License-Identifier: MIT
//! The "oneios,bridge" device (C: devices/bridge_dev.c): the host end of drivers/bridge/oneios_bridge_proto.h.
//! Messages from the guest are dispatched to the haptic, Bluetooth, brightness, battery and thermal bridges; messages
//! for the guest wait in a backlog until it has posted a buffer.

use crate::battery::{self, Battery};
use crate::brightness::{AlsSample, Bright};
use crate::bt::hci::{self, Hci, HciState, RxFn, StateFn};
use crate::haptics::Haptic;
use crate::proto::*;
use crate::thermal::{self, Thermal};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use xh_core::virtio::{self, Device, Info, Viov, Virtio};

const BACKLOG: usize = 256;
const BT_ADDRESS: &str = "/mnt/vendor/efs/bluetooth/bt_addr";

/// What makes the HCI controller when the guest first opens it (bt::hci_hidl::open_hidl on the phone).
pub type HciFactory = Box<dyn Fn(RxFn, StateFn) -> Option<Arc<Hci>> + Send + Sync>;

/// What the bridge serves: a channel whose part is None is not served.
#[derive(Default)]
pub struct BridgeConfig {
    pub haptic: Option<Arc<Haptic>>,
    pub hci_factory: Option<HciFactory>,
    /// None: /mnt/vendor/efs/bluetooth/bt_addr
    pub bt_address_path: Option<String>,
    pub brightness: Option<Arc<Bright>>,
    pub battery: Option<Arc<Battery>>,
    pub thermal: Option<Arc<Thermal>>,
}

/// The device's state.
pub struct BridgeDevice {
    config: BridgeConfig,
    hci: RefCell<Option<Arc<Hci>>>,
    backlog: RefCell<VecDeque<Vec<u8>>>,
    dropped: Cell<u64>,
    me: RefCell<Weak<Virtio<BridgeDevice>>>,
}

/// The one bridge of a session, for [`request_shutdown`] (the host has at most one).
static ACTIVE: Mutex<Option<Weak<Virtio<BridgeDevice>>>> = Mutex::new(None);

impl BridgeDevice {
    /// Writes the backlog into the guest's buffers while it has any (the device's lock held).
    fn drain(&self, dev: &Virtio<BridgeDevice>) {
        let mut iov = [Viov::EMPTY; 4];
        loop {
            let Some(message) = self.backlog.borrow().front().cloned() else { return };
            let Some((head, n)) = dev.pop(OB_QUEUE_RX, &mut iov) else { return };
            let mut written = virtio::write_chain(&iov[..n], 0, &message);
            if written != message.len() {
                written = 0; // too small a buffer: the message is lost
            }
            dev.push(OB_QUEUE_RX, head, written as u32);
            self.backlog.borrow_mut().pop_front();
        }
    }
}

/// A message for the guest: into the backlog, then as many as there are buffers.
fn to_guest(dev: &Virtio<BridgeDevice>, channel: u16, kind: u16, payload: &[u8]) {
    let s = dev.lock();
    if payload.len() > OB_MAX_PAYLOAD as usize {
        s.dropped.set(s.dropped.get() + 1);
        return;
    }
    {
        let mut backlog = s.backlog.borrow_mut();
        if backlog.len() == BACKLOG {
            backlog.pop_front(); // the oldest is lost
            s.dropped.set(s.dropped.get() + 1);
        }
        backlog.push_back(message(channel, kind, payload));
    }
    s.drain(dev);
}

/// A sender for the bridges' threads: the device while it lives.
fn sender(dev: &Virtio<BridgeDevice>) -> impl Fn(u16, u16, &[u8]) + Send + Sync + Clone + 'static {
    let weak = dev.lock().me.borrow().clone();
    move |channel, kind, payload: &[u8]| {
        if let Some(dev) = weak.upgrade() {
            to_guest(&dev, channel, kind, payload);
        }
    }
}

fn hci_state_message(state: HciState, address_path: &str) -> Vec<u8> {
    let mut m = ObHciState { state: state as u32, ..Default::default() };
    if state == HciState::On {
        if let Some(a) = hci::bt_address(address_path) {
            m.bd_addr = a;
        }
    }
    m.to_bytes()
}

fn battery_message(b: &battery::Sample) -> Vec<u8> {
    let flag = |on: bool, f: u32| if on { f } else { 0 };
    ObBatteryState {
        flags: flag(b.present, OB_BATTERY_PRESENT)
            | flag(b.plugged, OB_BATTERY_PLUGGED)
            | flag(b.charging, OB_BATTERY_CHARGING)
            | flag(b.full, OB_BATTERY_FULL),
        percent: b.percent,
        temp_deci_c: b.temp_deci_c,
        voltage_mv: b.voltage_mv,
        current_ma: b.current_ma,
        full_mah: b.full_mah,
        design_mah: b.design_mah,
        cycles: b.cycles,
        minutes: b.minutes,
    }
    .to_bytes()
}

fn thermal_message(t: &thermal::State) -> Vec<u8> {
    let r = &t.raw;
    ObThermalState {
        level: t.level,
        reason: t.reason,
        host_step: t.cap,
        case_temp: r.skin,
        soc: r.soc,
        cpu: r.cpu,
        gpu: r.gpu,
        ddr: r.ddr,
        xo: r.xo,
        board: r.ap,
        battery: r.battery,
    }
    .to_bytes()
}

impl BridgeDevice {
    fn address_path(&self) -> &str {
        self.config.bt_address_path.as_deref().unwrap_or(BT_ADDRESS)
    }

    fn hci_state(&self, dev: &Virtio<BridgeDevice>, state: HciState) {
        to_guest(dev, OB_CH_HCI, OB_HCI_STATE, &hci_state_message(state, self.address_path()));
    }

    fn bright_state(&self, dev: &Virtio<BridgeDevice>, b: &Bright) {
        let info = b.info();
        // The lowest level's light: the floor's share of the range, of max_nits (the node is close to linear in light).
        let min_millinits = if info.ceiling != 0 {
            u64::from(info.max_nits) * 1000 * u64::from(info.floor) / u64::from(info.ceiling)
        } else {
            0
        };
        let flags = if info.panel { OB_BRIGHT_HAS_PANEL } else { 0 } | if info.als { OB_BRIGHT_HAS_ALS } else { 0 };
        let state =
            ObBrightState { flags, max_nits: info.max_nits, min_millinits: min_millinits as u32, level: b.level() };
        to_guest(dev, OB_CH_BRIGHTNESS, OB_BRIGHT_STATE, &state.to_bytes());
    }

    fn hello(&self, dev: &Virtio<BridgeDevice>) {
        let c = &self.config;
        let bit = |on: bool, ch: u16| if on { 1u32 << ch } else { 0 };
        let channels = 1 << OB_CH_CONTROL
            | bit(c.haptic.is_some(), OB_CH_HAPTIC)
            | bit(c.hci_factory.is_some(), OB_CH_HCI)
            | bit(c.brightness.is_some(), OB_CH_BRIGHTNESS)
            | bit(c.battery.is_some(), OB_CH_BATTERY)
            | bit(c.thermal.is_some(), OB_CH_THERMAL);
        let reply = if c.haptic.as_ref().is_some_and(|h| h.impacts_available()) {
            ObCtlHelloExt { version: OB_PROTO_VERSION, channels, features: OB_FEAT_HAPTIC_IMPACT }.to_bytes()
        } else {
            ObCtlHello { version: OB_PROTO_VERSION, channels }.to_bytes()
        };
        to_guest(dev, OB_CH_CONTROL, OB_CTL_HELLO, &reply);
    }

    fn haptic(&self, h: &ObHeader, payload: &[u8]) {
        let Some(haptic) = &self.config.haptic else { return };
        match h.kind {
            OB_HAPTIC_VIB => {
                if let Some(v) = ObHapticVib::parse(payload) {
                    haptic.vib(v.on_ms, v.intensity.into(), v.frequency_hz.into());
                }
            }
            OB_HAPTIC_STOP => haptic.stop(),
            OB_HAPTIC_IMPACT if payload.len() == ObHapticImpact::SIZE && haptic.impacts_available() => {
                let impact = ObHapticImpact::read_from(payload);
                if (OB_IMPACT_LIGHT..=OB_IMPACT_HEAVY).contains(&impact.kind) {
                    let _ = haptic.impact(impact.kind);
                }
                // Malformed/unsupported impacts are never silently mapped to VIB.
            }
            _ => {}
        }
    }

    fn hci(&self, dev: &Virtio<BridgeDevice>, h: &ObHeader, payload: &[u8]) {
        let Some(factory) = &self.config.hci_factory else { return };
        match h.kind {
            OB_HCI_OPEN => {
                let existing = self.hci.borrow().clone();
                let hci = match existing {
                    None => {
                        let send = sender(dev);
                        let rx: RxFn = Box::new({
                            let send = send.clone();
                            move |kind, packet: &[u8]| send(OB_CH_HCI, kind.into(), packet)
                        });
                        let path = self.address_path().to_owned();
                        let state: StateFn =
                            Box::new(move |s| send(OB_CH_HCI, OB_HCI_STATE, &hci_state_message(s, &path)));
                        let made = factory(rx, state);
                        self.hci.borrow_mut().clone_from(&made);
                        made
                    }
                    Some(hci) => {
                        // Failed after it was opened (the HAL died, or never finished initialising): a new client starts
                        // over.
                        if hci.state() == HciState::Failed {
                            hci.close();
                        }
                        Some(hci)
                    }
                };
                match hci {
                    Some(hci) if hci.open().is_ok() => {
                        if hci.state() == HciState::On {
                            self.hci_state(dev, HciState::On); // already open: say so again
                        }
                    }
                    _ => self.hci_state(dev, HciState::Failed),
                }
            }
            OB_HCI_CLOSE => {
                if let Some(hci) = self.hci.borrow().clone() {
                    hci.close();
                }
            }
            OB_HCI_COMMAND..=OB_HCI_ISO => {
                if let Some(hci) = self.hci.borrow().clone() {
                    let _ = hci.send(h.kind as u8, payload);
                }
            }
            _ => {}
        }
    }

    fn brightness(&self, dev: &Virtio<BridgeDevice>, h: &ObHeader, payload: &[u8]) {
        let Some(b) = &self.config.brightness else { return };
        match h.kind {
            OB_BRIGHT_SET => {
                if let Some(set) = ObBrightSet::parse(payload) {
                    b.set(set.level, set.flags & OB_BRIGHT_LINEAR != 0, set.fade_ms);
                }
            }
            OB_BRIGHT_POWER => {
                if let Some(p) = ObBrightPower::parse(payload) {
                    let why =
                        if p.source == OB_POWER_FRAMEBUFFER { "the framebuffer's power request" } else { "the guest" };
                    b.display(p.on != 0, Some(why));
                    // the sensor hub's streams stop while iOS's display is off (runtime/bridges/sensors)
                    crate::sensors::display(p.on != 0);
                }
            }
            OB_BRIGHT_QUERY => {
                self.bright_state(dev, b);
                let send = sender(dev);
                // light readings from now on
                b.listen(Some(Arc::new(move |s: &AlsSample| {
                    let als = ObBrightAls { millilux: s.millilux, clear: s.clear, ir: s.ir, uv: s.uv };
                    send(OB_CH_BRIGHTNESS, OB_BRIGHT_ALS, &als.to_bytes());
                })));
            }
            _ => {}
        }
    }

    fn dispatch(&self, dev: &Virtio<BridgeDevice>, h: &ObHeader, payload: &[u8]) {
        match h.channel {
            OB_CH_CONTROL if h.kind == OB_CTL_HELLO => self.hello(dev),
            OB_CH_HAPTIC => self.haptic(h, payload),
            OB_CH_HCI => self.hci(dev, h, payload),
            OB_CH_BRIGHTNESS => self.brightness(dev, h, payload),
            OB_CH_BATTERY if h.kind == OB_BATTERY_QUERY => {
                if let Some(bat) = &self.config.battery {
                    // on a failed read: the last good sample
                    let sample = bat.query().unwrap_or_else(|last| last);
                    to_guest(dev, OB_CH_BATTERY, OB_BATTERY_STATE, &battery_message(&sample));
                    let send = sender(dev);
                    // changes from now on
                    bat.listen(Some(Arc::new(move |b: &battery::Sample| {
                        send(OB_CH_BATTERY, OB_BATTERY_STATE, &battery_message(b))
                    })));
                }
            }
            OB_CH_THERMAL if h.kind == OB_THERMAL_QUERY => {
                if let Some(t) = &self.config.thermal {
                    let state = t.get(); // the worker reads every 5 s from xnuhost's start
                    let send = sender(dev);
                    // changes from now on
                    t.on_report(Some(Arc::new(move |s: &thermal::State| {
                        send(OB_CH_THERMAL, OB_THERMAL_STATE, &thermal_message(s))
                    })));
                    to_guest(dev, OB_CH_THERMAL, OB_THERMAL_STATE, &thermal_message(&state));
                }
            }
            _ => {}
        }
    }
}

impl Device for BridgeDevice {
    fn handle(&self, dev: &Virtio<Self>, queue: u32, _head: u16, iov: &[Viov]) -> Option<u32> {
        if queue != OB_QUEUE_TX {
            return Some(0);
        }
        // A part longer than any message is ignored (as the whole message).
        let mut size = 0usize;
        for part in iov.iter().filter(|p| !p.writable()) {
            size += part.len() as usize;
            if size > OB_MAX_MESSAGE as usize {
                return Some(0);
            }
        }
        let mut message = vec![0u8; size];
        virtio::read_chain(iov, 0, &mut message);
        let Some(header) = ObHeader::parse(&message) else { return Some(0) }; // None would keep the chain
        let body = &message[ObHeader::SIZE..];
        if header.length as usize > body.len() {
            return Some(0);
        }
        self.dispatch(dev, &header, &body[..header.length as usize]);
        Some(0)
    }

    fn kick(&self, dev: &Virtio<Self>, _queue: u32) {
        self.drain(dev);
    }

    fn reset(&self, _dev: &Virtio<Self>) {
        self.backlog.borrow_mut().clear();
        if let Some(h) = &self.config.haptic {
            h.stop(); // a guest that resets (or panics into a reset) leaves no motor running
        }
        if let Some(hci) = self.hci.borrow().clone() {
            hci.close();
        }
        if let Some(b) = &self.config.battery {
            b.listen(None); // no reads and no backlog for a guest that is gone
        }
        if let Some(t) = &self.config.thermal {
            t.on_report(None); // the reads go on: xnuhost's own step needs them
        }
    }

    fn config_read(&self, _dev: &Virtio<Self>, offset: u32, _size: u32) -> u32 {
        if offset == 0 {
            OB_PROTO_VERSION
        } else {
            0
        }
    }
}

/// "oneios,bridge": the message channel of drivers/bridge. None when the device cannot be made.
pub fn create(base: u64, spi: u32, config: BridgeConfig) -> Option<Arc<Virtio<BridgeDevice>>> {
    let info = Info { device_id: OB_VIRTIO_DEVICE_ID, nqueues: 2, manual: 1 << OB_QUEUE_RX, ..Info::default() };
    let device = BridgeDevice {
        config,
        hci: RefCell::new(None),
        backlog: RefCell::new(VecDeque::new()),
        dropped: Cell::new(0),
        me: RefCell::new(Weak::new()),
    };
    let dev = Virtio::create(base, spi, info, device, "oneios-bridge").ok()?;
    *dev.lock().me.borrow_mut() = Arc::downgrade(&dev);
    *ACTIVE.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::downgrade(&dev));
    Some(dev)
}

/// The return's graceful-shutdown ask (OB_CTL_SHUTDOWN on the control channel). Queued like any host->guest message
/// (it waits in the backlog until the guest's driver reads it); false when there is no bridge to ask through. The
/// caller does not depend on the guest acting: it kills at return-graceful-ms regardless.
pub fn request_shutdown() -> bool {
    let active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner).as_ref().and_then(Weak::upgrade);
    let Some(dev) = active else { return false };
    to_guest(&dev, OB_CH_CONTROL, OB_CTL_SHUTDOWN, &[]);
    true
}

impl BridgeDevice {
    /// Messages lost: too long, the backlog full.
    pub fn dropped(&self) -> u64 {
        self.dropped.get()
    }
}
