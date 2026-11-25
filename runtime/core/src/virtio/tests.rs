// SPDX-License-Identifier: MIT
// The transport driven as a guest driver would (test_xnuhost.c's driver and test_health, test_bridges.c's guest).
use super::testing::{self, Driver, GuestRam};
use super::*;
use std::cell::{Cell, RefCell};

/// A device whose queue 0 echoes (copies the readable bytes into the writable parts) and whose queue 1 keeps every
/// chain (completed later with push); queue 2 is manual.
#[derive(Default)]
struct Echo {
    kept: RefCell<Vec<u16>>,
    kicks: Cell<u32>,
    resets: Cell<u32>,
    config: Cell<u32>,
}

impl Device for Echo {
    fn handle(&self, _dev: &Virtio<Self>, queue: u32, head: u16, iov: &[Viov]) -> Option<u32> {
        if queue == 1 {
            self.kept.borrow_mut().push(head);
            return None;
        }
        let mut data = vec![0; readable_len(iov)];
        read_chain(iov, 0, &mut data);
        Some(write_chain(iov, 0, &data) as u32)
    }
    fn config_read(&self, _dev: &Virtio<Self>, offset: u32, _size: u32) -> u32 {
        if offset == 0 {
            self.config.get()
        } else {
            0
        }
    }
    fn config_write(&self, dev: &Virtio<Self>, offset: u32, _size: u32, value: u32) {
        if offset == 0 {
            self.config.set(value);
            dev.config_changed(); // the lock again, from a callback
        }
    }
    fn reset(&self, _dev: &Virtio<Self>) {
        self.resets.set(self.resets.get() + 1);
    }
    fn kick(&self, _dev: &Virtio<Self>, _queue: u32) {
        self.kicks.set(self.kicks.get() + 1);
    }
}

const ECHO: Info = Info { device_id: 99, features: 1 << 5, nqueues: 3, manual: 1 << 2 };

fn echo(spi: u32, registry: &Registry) -> Arc<Virtio<Echo>> {
    testing::install();
    Virtio::create_in(registry, 0, spi, ECHO, Echo::default(), "virtio-echo").unwrap()
}

fn up(dev: &Arc<Virtio<Echo>>, ram: &GuestRam, features: u64) -> Option<Driver> {
    Driver::up(dev.clone(), features, 3, 8, ram.pa(0))
}

#[test]
fn identity_registers() {
    let reg = Registry::new();
    let dev = echo(60, &reg);
    assert_eq!(dev.mmio_read(0, 4), 0x7472_6976);
    assert_eq!(dev.mmio_read(4, 4), 2);
    assert_eq!(dev.mmio_read(8, 4), 99);
    assert_eq!(dev.mmio_read(0xc, 4), 0x4f69_4f53);
    dev.mmio_write(0x14, 4, 0);
    assert_eq!(dev.mmio_read(0x10, 4), 1 << 5);
    dev.mmio_write(0x14, 4, 1);
    assert_eq!(dev.mmio_read(0x10, 4), 1);
    dev.mmio_write(0x14, 4, 2);
    assert_eq!(dev.mmio_read(0x10, 4), 0);
    dev.mmio_write(0x30, 4, 2);
    assert_eq!(dev.mmio_read(0x34, 4), u64::from(QSIZE));
    dev.mmio_write(0x30, 4, 3);
    assert_eq!(dev.mmio_read(0x34, 4), 0);
    assert_eq!((dev.label(), dev.name(), dev.spi()), ("echo#0", "virtio-echo", 60));
    let second = Virtio::create_in(&reg, 0, 61, ECHO, Echo::default(), "virtio-echo").unwrap();
    assert_eq!(second.label(), "echo#1");
}

#[test]
fn features_ok_needs_version_1_and_nothing_extra() {
    let reg = Registry::new();
    let dev = echo(62, &reg);
    let ram = GuestRam::new(0x10000);
    // no VERSION_1: FEATURES_OK refused
    dev.mmio_write(0x70, 4, 0);
    dev.mmio_write(0x24, 4, 1);
    dev.mmio_write(0x20, 4, 0);
    dev.mmio_write(0x70, 4, 11);
    assert_eq!(dev.mmio_read(0x70, 4) & 8, 0);
    // a feature the device lacks: refused
    assert!(up(&dev, &ram, 1 << 6).is_none());
    // what it offers: up
    assert!(up(&dev, &ram, 1 << 5).is_some());
    assert!(dev.ready());
    assert_eq!(dev.driver_features(), F_VERSION_1 | 1 << 5);
}

#[test]
fn echo_chain_and_interrupt() {
    let reg = Registry::new();
    let dev = echo(63, &reg);
    let ram = GuestRam::new(0x20000);
    let mut d = up(&dev, &ram, 0).unwrap();
    ram.write(0x10000, b"hello, guest");
    let raised = testing::spi_raised(63);
    let len = d.send(0, &[(ram.pa(0x10000), 5, false), (ram.pa(0x10005), 7, false), (ram.pa(0x11000), 64, true)], true);
    assert_eq!(len, Some(12));
    assert_eq!(ram.read(0x11000, 12), b"hello, guest");
    assert_eq!(testing::spi_raised(63), raised + 1);
    assert!(testing::spi_level(63));
    assert_eq!(dev.mmio_read(0x60, 4), 1);
    dev.mmio_write(0x64, 4, 1); // the driver acks: the line goes down
    assert!(!testing::spi_level(63));
    // a chain with a part outside RAM is bad: returned unused
    assert_eq!(d.send(0, &[(0x100, 4, false)], true), Some(0));
    // single parts in a row, round the ring
    for i in 0..20u8 {
        ram.write(0x12000, &[i]);
        assert_eq!(d.send(0, &[(ram.pa(0x12000), 1, false), (ram.pa(0x13000), 1, true)], true), Some(1));
        assert_eq!(ram.read(0x13000, 1), [i]);
    }
}

#[test]
fn indirect_table() {
    let reg = Registry::new();
    let dev = echo(64, &reg);
    let ram = GuestRam::new(0x20000);
    let mut d = up(&dev, &ram, 0).unwrap();
    ram.write(0x10000, b"abc");
    // an indirect table of two descriptors at 0x14000
    let t = 0x14000;
    ram.set_u64(t, ram.pa(0x10000));
    ram.set_u32(t + 8, 3);
    ram.set_u16(t + 12, DESC_NEXT);
    ram.set_u16(t + 14, 1);
    ram.set_u64(t + 16, ram.pa(0x11000));
    ram.set_u32(t + 24, 8);
    ram.set_u16(t + 28, DESC_WRITE);
    // the queue's descriptor 0 points at it
    let q = d.ring_pa(0) - ram.pa(0);
    let q = q as usize;
    ram.set_u64(q, ram.pa(t));
    ram.set_u32(q + 8, 32);
    ram.set_u16(q + 12, DESC_INDIRECT);
    ram.set_u16(q + 0x1004, 0);
    ram.set_u16(q + 0x1002, 1);
    d.kick(0);
    assert_eq!(d.reap(0), Some((0, 3)));
    assert_eq!(ram.read(0x11000, 3), b"abc");
}

#[test]
fn kept_chains_and_manual_queue() {
    let reg = Registry::new();
    let dev = echo(65, &reg);
    let ram = GuestRam::new(0x20000);
    let mut d = up(&dev, &ram, 0).unwrap();
    // queue 1 keeps the chain: nothing used until the device pushes it
    assert_eq!(d.send(1, &[(ram.pa(0x10000), 8, true)], true), None);
    let head = dev.lock().kept.borrow_mut().pop().unwrap();
    dev.push(1, head, 8);
    assert_eq!(d.reap(1), Some((u32::from(head), 8)));
    // queue 2 is the device's: the kick only tells it; it pops when it has data
    assert_eq!(d.send(2, &[(ram.pa(0x11000), 16, true)], true), None);
    assert_eq!(dev.lock().kicks.get(), 1);
    let mut iov = [Viov::EMPTY; 4];
    let (head, n) = dev.pop(2, &mut iov).unwrap();
    assert_eq!((n, iov[0].len(), iov[0].writable()), (1, 16, true));
    assert_eq!(iov[0].write_at(0, b"event!"), 6);
    assert_eq!(iov[0].fill(6, b'.', 100), 10);
    dev.push(2, head, 6);
    assert_eq!(d.reap(2), Some((u32::from(head), 6)));
    assert_eq!(ram.read(0x11000, 8), b"event!..");
    assert!(dev.pop(2, &mut iov).is_none()); // nothing more posted
                                             // a bad chain popped by the device is returned unused and gives None
    d.send(2, &[(0x100, 4, true)], false);
    assert!(dev.pop(2, &mut iov).is_none());
    assert_eq!(d.reap(2).map(|(_, len)| len), Some(0));
    // a queue beyond nqueues: the kick is ignored
    d.kick(3);
}

#[test]
fn recursive_lock_from_device_threads() {
    let reg = Registry::new();
    let dev = echo(66, &reg);
    let ram = GuestRam::new(0x20000);
    let mut d = up(&dev, &ram, 0).unwrap();
    for i in 0..4 {
        d.send(2, &[(ram.pa(0x10000 + i * 0x100), 4, true)], false);
    }
    let worker = {
        let dev = dev.clone();
        std::thread::spawn(move || {
            let mut done = 0;
            while done < 4 {
                let g = dev.lock(); // the device's state, then pop/push under it (the lock nests)
                let mut iov = [Viov::EMPTY; 2];
                if let Some((head, _)) = dev.pop(2, &mut iov) {
                    g.kicks.set(g.kicks.get() + 1);
                    iov[0].write_at(0, &[0xaa; 4]);
                    dev.push(2, head, 4);
                    done += 1;
                }
            }
        })
    };
    worker.join().unwrap();
    let mut got = 0;
    while d.reap(2).is_some() {
        got += 1;
    }
    assert_eq!(got, 4);
    assert_eq!(ram.read(0x10300, 4), [0xaa; 4]);
}

#[test]
fn config_and_reset() {
    let reg = Registry::new();
    let dev = echo(67, &reg);
    let ram = GuestRam::new(0x20000);
    let d = up(&dev, &ram, 0).unwrap();
    let generation = d.read(0xfc);
    d.write(0x100, 42); // config_write calls config_changed under the lock
    assert_eq!(d.read(0x100), 42);
    assert_eq!(d.read(0xfc), generation + 1);
    assert_eq!(d.read(0x60) & ISR_CONFIG, ISR_CONFIG);
    // queue size: a power of two up to QSIZE only
    d.write(0x30, 0);
    d.write(0x38, 3);
    d.write(0x38, 2048);
    // reset: state cleared, the device told, the line lowered
    let ((), lines) = crate::log::capture(|| {
        set_log(true);
        d.write(0x70, 0);
        set_log(false);
    });
    assert!(lines.iter().any(|(_, l)| l == "virtio echo#0: reset by the guest's driver (it was up)"), "{lines:?}");
    assert_eq!(dev.lock().resets.get(), 2); // Driver::up resets first
    assert!(!dev.ready() && !testing::spi_level(67));
    assert_eq!((d.read(0x70), d.read(0x60), d.read(0x44)), (0, 0, 0));
    assert_eq!(d.read(0xfc), generation + 1); // the generation is not the reset's
}

#[test]
fn state_lines() {
    let reg = Registry::new();
    let dev = echo(68, &reg);
    let ram = GuestRam::new(0x20000);
    let (d, lines) = crate::log::capture(|| {
        set_log(true);
        let d = up(&dev, &ram, 1 << 5);
        dev.mmio_write(0x70, 4, 15 | 128);
        set_log(false);
        d
    });
    assert!(d.is_some());
    let texts: Vec<&str> = lines.iter().map(|(_, l)| l.as_str()).collect();
    assert!(
        texts.contains(&"virtio echo#0: the guest's driver is up (features 0x100000020, 3 queue(s) ready)"),
        "{texts:?}"
    );
    assert!(texts.contains(&"virtio echo#0: the guest's driver set FAILED (status 0x8f)"), "{texts:?}");
}

#[test]
fn chain_helpers() {
    let mut a = *b"0123";
    let mut b = *b"4567";
    let mut w = [0u8; 6];
    // SAFETY: the arrays outlive the parts.
    let iov = unsafe {
        [
            Viov::from_raw(a.as_mut_ptr(), 4, false),
            Viov::from_raw(w.as_mut_ptr(), 3, true),
            Viov::from_raw(b.as_mut_ptr(), 4, false),
            Viov::from_raw(w.as_mut_ptr().add(3), 3, true),
        ]
    };
    assert_eq!((readable_len(&iov), writable_len(&iov)), (8, 6));
    let mut out = [0u8; 5];
    assert_eq!(read_chain(&iov, 2, &mut out), 5);
    assert_eq!(&out, b"23456");
    assert_eq!(write_chain(&iov, 1, b"abcdefgh"), 5);
    assert_eq!(&w, b"\0abcde");
    assert_eq!(iov[0].write_at(0, b"x"), 0); // a readable part is not written
}

// test_xnuhost.c test_health: a device whose queue 0 holds the chains it is given.
#[derive(Default)]
struct Keep;
impl Device for Keep {
    fn handle(&self, _dev: &Virtio<Self>, _queue: u32, _head: u16, _iov: &[Viov]) -> Option<u32> {
        None
    }
}

#[test]
fn health() {
    testing::install();
    let reg = Registry::new();
    let info = Info { device_id: 99, nqueues: 1, ..Info::default() };
    let dev = Virtio::create_in(&reg, 0, 50, info, Keep, "virtio-testq").unwrap();
    let ram = GuestRam::new(0x20000);
    let mut d = Driver::up(dev.clone(), 0, 1, 8, ram.pa(0)).unwrap();
    let report = |now: u64| {
        let mut lines = String::new();
        reg.health(now, &mut |l| {
            lines.push_str(l);
            lines.push('\n');
        });
        lines
    };
    report(1000); // the baseline
    assert!(!report(61000).contains("testq")); // nothing moved: not named
    d.send(0, &[(ram.pa(0x8000), 8, true)], true);
    d.send(0, &[(ram.pa(0x8008), 8, true)], true);
    let l = report(121000);
    assert!(l.contains("virtio: 60 s: ") && l.contains("testq#0 q0 +0 (in flight 2)"), "{l}");
    assert!(!l.contains("virtio testq#0")); // taken in this span: no episode yet
    let l = report(181000);
    assert!(
        l.contains("virtio testq#0: queue 0: 2 chain(s) in flight and none returned in 60 s (taken 2, returned 0 since the boot)"),
        "{l}"
    );
    assert!(!l.contains("testq#0 q0")); // nothing moved: not in the summary
    assert!(!report(241000).contains("testq")); // told once
    dev.push(0, 0, 8); // the device answers one
    let l = report(301000);
    assert!(
        l.contains("testq#0 q0 +1 (in flight 1)")
            && l.contains("virtio testq#0: queue 0 returns chains again (after 180 s)"),
        "{l}"
    );
    // posted and never taken (a queue the transport pops on the kick): the guest posts without a kick
    let info2 = Info { device_id: 98, nqueues: 1, ..Info::default() };
    let dev2 = Virtio::create_in(&reg, 0, 51, info2, Keep, "virtio-testq").unwrap();
    let ram2 = GuestRam::new(0x20000);
    let mut e = Driver::up(dev2.clone(), 0, 1, 8, ram2.pa(0)).unwrap();
    e.send(0, &[(ram2.pa(0x8000), 8, true)], false);
    let l = report(361000) + &report(421000);
    assert!(l.contains("virtio testq#1: queue 0: 1 chain(s) posted by the guest and not taken in 60 s"), "{l}");
}

#[test]
fn health_summary_and_bad_chains() {
    testing::install();
    let reg = Registry::new();
    let a = Virtio::create_in(&reg, 0, 52, ECHO, Echo::default(), "virtio-gpu").unwrap();
    let b = Virtio::create_in(&reg, 0, 53, ECHO, Echo::default(), "virtio-input").unwrap();
    let ram = GuestRam::new(0x40000);
    let mut da = up(&a, &ram, 0).unwrap();
    let mut db = Driver::up(b.clone(), 0, 3, 8, ram.pa(0x20000)).unwrap();
    let mut lines = Vec::new();
    reg.health(1000, &mut |l| lines.push(l.to_owned()));
    for _ in 0..3 {
        da.send(0, &[(ram.pa(0x10000), 4, true)], true);
    }
    da.send(1, &[(ram.pa(0x10000), 4, true)], true);
    db.send(0, &[(0x200, 4, false)], true); // bad
    lines.clear();
    reg.health(61400, &mut |l| lines.push(l.to_owned()));
    assert_eq!(lines, ["virtio: 60 s: gpu#0 q0 +3 q1 +0 (in flight 1), input#0 q0 +1; bad chains +1"]);
}

#[test]
fn bus_registration() {
    testing::install();
    let reg = Registry::new();
    let base = 0x7e00_0000;
    let dev = Virtio::create_in(&reg, base, 54, ECHO, Echo::default(), "virtio-echo").unwrap();
    assert!(crate::mmio::bus().covers(base) && crate::mmio::bus().covers(base + 0xfff));
    assert_eq!(crate::mmio::bus().read(base + 8, 4), 99);
    assert!(Virtio::create_in(&reg, base, 55, ECHO, Echo::default(), "virtio-echo").is_err());
    drop(dev);
}

// The transport's own lines (say), whatever set_log is: for the tests that look for them.
static SAID: Mutex<String> = Mutex::new(String::new());

pub(super) fn said(args: std::fmt::Arguments<'_>) {
    use std::fmt::Write as _;
    let mut s = SAID.lock().unwrap_or_else(|e| e.into_inner());
    let _ = s.write_fmt(args);
    s.push('\n');
}

fn said_lines() -> String {
    SAID.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

// test_xnuhost.c test_health_held: a handler that does not come back (phone, 2026-11-23: the encoder's open waited for
// ever on cpu1 with the virtio-media device held, and the main loop's report waited in its lock): the report goes on,
// says who holds the device, and says when it is free again.
struct Stuck {
    entered: Mutex<std::sync::mpsc::Sender<()>>,
    go: Mutex<std::sync::mpsc::Receiver<()>>,
}
impl Device for Stuck {
    fn handle(&self, _dev: &Virtio<Self>, _queue: u32, _head: u16, _iov: &[Viov]) -> Option<u32> {
        let _ = self.entered.lock().unwrap().send(());
        let _ = self.go.lock().unwrap().recv(); // until the test lets it go
        Some(0)
    }
}

#[test]
fn health_held() {
    testing::install();
    hooks::install_cpu(|| Some(1)); // the guest's thread below is "cpu1" (no other test looks at the vCPU)
    let reg = Registry::new();
    reg.set_held_ms(0);
    let (go, wait) = std::sync::mpsc::channel();
    let (entered_tx, entered) = std::sync::mpsc::channel();
    let stuck = Stuck { entered: Mutex::new(entered_tx), go: Mutex::new(wait) };
    let info = Info { device_id: 97, nqueues: 1, ..Info::default() };
    let dev = Virtio::create_in(&reg, 0, 52, info, stuck, "virtio-stuckq").unwrap();
    let ram = GuestRam::new(0x20000);
    let mut d = Driver::up(dev.clone(), 0, 1, 8, ram.pa(0)).unwrap();
    let report = || {
        let mut lines = String::new();
        reg.health(crate::time::monotonic_ms(), &mut |l| {
            lines.push_str(l);
            lines.push('\n');
        });
        lines
    };
    let pa = ram.pa(0x8000);
    let guest = std::thread::spawn(move || {
        d.send(0, &[(pa, 8, true)], true); // the kick runs the handler here
    });
    // The guest's thread is in the handler, the device's lock held: the report must come back all the same.
    entered.recv().unwrap();
    let l = report();
    assert!(
        l.contains("virtio stuckq#0: held by a request on queue 0 for ")
            && l.contains(" s (cpu1 in its handler): the guest's next access to the device waits for it"),
        "{l}"
    );
    assert!(!report().contains("stuckq"), "said once per hold");
    go.send(()).unwrap();
    guest.join().unwrap();
    assert!(said_lines().contains("virtio stuckq#0: free again: the request that held it ended after "));
    let l = report();
    assert!(l.contains("stuckq#0 q0 +1"), "its counts come with the next report: {l}");
    // A device thread's own lock: "held by cpu1" without a queue (the hook says cpu1 here too), and free again.
    let g = dev.lock();
    let other = std::thread::scope(|s| s.spawn(report).join().unwrap());
    assert!(other.contains("virtio stuckq#0: held by cpu1 for 0 s: the guest's next access"), "{other}");
    drop(g);
    hooks::install_cpu(|| None);
}

// A held device keeps its bad chains in the sum (C left them out: the report said a wrapped "bad chains +N").
#[test]
fn health_held_keeps_bad_chains() {
    testing::install();
    let reg = Registry::new();
    let a = Virtio::create_in(&reg, 0, 56, ECHO, Echo::default(), "virtio-gpu").unwrap();
    let b = Virtio::create_in(&reg, 0, 57, ECHO, Echo::default(), "virtio-input").unwrap();
    let ram = GuestRam::new(0x40000);
    let mut da = up(&a, &ram, 0).unwrap();
    let mut db = Driver::up(b.clone(), 0, 3, 8, ram.pa(0x20000)).unwrap();
    let report = |now: u64| {
        let mut lines = Vec::new();
        reg.health(now, &mut |l| lines.push(l.to_owned()));
        lines
    };
    report(1000);
    db.send(0, &[(0x200, 4, false)], true); // bad
    assert_eq!(report(61000), ["virtio: 60 s: input#0 q0 +1; bad chains +1"]);
    da.send(0, &[(ram.pa(0x10000), 4, true)], true);
    let held = b.lock(); // input#0 held on this thread; the report runs on another
    let lines = std::thread::scope(|s| s.spawn(|| report(121000)).join().unwrap());
    drop(held);
    assert_eq!(lines, ["virtio: 60 s: gpu#0 q0 +1"]);
}

/// A device whose queue 0 honours the driver's VIRTQ_AVAIL_F_NO_INTERRUPT (the media device's queues, 2026-11-25).
struct Quiet;

impl Device for Quiet {
    fn quiet_queues(&self) -> u32 {
        1
    }
}

#[test]
fn no_interrupt_is_honoured_only_on_the_queues_the_device_names() {
    testing::install();
    let reg = Registry::new();
    let info = Info { device_id: 96, nqueues: 2, ..Info::default() };
    let dev = Virtio::create_in(&reg, 0, 70, info, Quiet, "virtio-quiet").unwrap();
    let ram = GuestRam::new(0x20000);
    let mut d = Driver::up(dev.clone(), 0, 2, 8, ram.pa(0)).unwrap();
    let flags = |q: u64| (q * testing::RING_BYTES + 0x1000) as usize; // each queue's avail ring flags
    let buf = [(ram.pa(0x10000), 4, false)];
    // queue 0, the flag set: returned with no interrupt status and no line
    ram.set_u16(flags(0), 1);
    let raised = testing::spi_raised(70);
    assert_eq!(d.send(0, &buf, true), Some(0));
    assert_eq!(d.used_idx(0), 1);
    assert_eq!((testing::spi_raised(70), testing::spi_level(70), dev.mmio_read(0x60, 4)), (raised, false, 0));
    // the flag cleared: the line rises, and goes down on the ack
    ram.set_u16(flags(0), 0);
    assert_eq!(d.send(0, &buf, true), Some(0));
    assert_eq!((testing::spi_raised(70), testing::spi_level(70), dev.mmio_read(0x60, 4)), (raised + 1, true, 1));
    dev.mmio_write(0x64, 4, 1);
    assert!(!testing::spi_level(70));
    // queue 1 is not the device's quiet one: the flag changes nothing (every other device's drivers rely on that)
    ram.set_u16(flags(1), 1);
    assert_eq!(d.send(1, &buf, true), Some(0));
    assert_eq!((testing::spi_raised(70), testing::spi_level(70)), (raised + 2, true));
    dev.mmio_write(0x64, 4, 1);
    // a device that names none (Echo) raises whatever the flag
    let echo_dev = echo(71, &reg);
    let ram2 = GuestRam::new(0x20000);
    let mut e = up(&echo_dev, &ram2, 0).unwrap();
    ram2.set_u16(0x1000, 1);
    let before = testing::spi_raised(71);
    assert_eq!(e.send(0, &[(ram2.pa(0x10000), 4, false)], true), Some(0));
    assert_eq!(testing::spi_raised(71), before + 1);
}
