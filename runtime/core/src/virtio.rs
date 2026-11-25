// SPDX-License-Identifier: MIT
//! The virtio-mmio transport, version 2 (VirtIO 1.2, section 4.2), split virtqueues (`virtio.c`): the device side of
//! what UniXNU's NativeVirtioBlock (<https://github.com/UniXNU/VirtioMmioBlock>, 4e1013e) and UniXNU-droidvm's virtio
//! drivers (package/DroidVMVirtio*.c) program. crosvm is that device side in a DroidVM; here it is this module, in the
//! same process as the kernel.
//!
//! A device model is a [`Device`] (its state and callbacks) put on a [`Virtio`] with [`Virtio::create`]. The
//! transport pops the chains of a queue on the guest's kick and hands each to [`Device::handle`]; a queue in
//! [`Info::manual`] is the device's own: the kick only tells it ([`Device::kick`]) and it takes the guest's buffers
//! with [`Virtio::pop`] when it has data. Guest memory is reached through [`crate::hooks::ram`], interrupts through
//! [`crate::hooks::set_spi`].
//!
//! The device's lock is recursive (a pthread recursive mutex, as in C): what the device's own threads take around its
//! state ([`Virtio::lock`]), so that it nests the same way in them as in the vCPU that handles a register access. Every
//! callback runs with it held, and [`Virtio::pop`], [`Virtio::push`] and the others take it again themselves. Because
//! it is recursive a holder gets only `&D`: a device keeps what it changes in `Cell`/`RefCell` fields (`D: Send` is
//! enough; the lock makes the sharing safe, as std's `ReentrantLock`).
//!
//! Health (lasting lines, docs/booter.md "Health lines"): each queue counts the chains taken from the guest and
//! returned to it; [`Registry::health`] prints them every 60 s when they moved, and once per episode a queue that holds
//! chains and returned none for a whole span, or (a queue the transport pops itself) whose posted chains nobody took.
//! The driver's state changes are one line each: up (DRIVER_OK), FAILED, a reset after it was up. All of these go to
//! the log only after [`set_log`]`(true)` (xnuhost's main does it; the bridges' tools and tests leave it off).
use crate::hooks;
use crate::mmio::{self, AddError, MmioDevice};
use std::cell::{RefCell, UnsafeCell};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// Queues a device may have (`XH_VIRTIO_QUEUES`).
pub const QUEUES: usize = 4;
/// The largest queue a driver may ask for (`XH_VIRTIO_QSIZE`). 1024 as crosvm offers: Droid-VM's virtio-gpu driver
/// needs 512 for its control queue (two descriptors for each of 256 slots) and refused the device at 256.
pub const QSIZE: u32 = 1024;
/// The health report's span (`XH_VIRTIO_REPORT_MS`).
pub const REPORT_MS: u64 = 60_000;
/// The parts of one chain the transport hands to [`Device::handle`] at most.
pub const MAX_PARTS: usize = 64;
/// A device held this long or longer by one holder is said by the health report (`xh_virtio_held_ms`); a test's
/// registry sets less ([`Registry::set_held_ms`]).
pub const HELD_MS: u64 = 2000;

const MAGIC: u32 = 0x7472_6976;
const VENDOR: u32 = 0x4f69_4f53; // "SOiO"
/// VIRTIO_F_VERSION_1, offered by every device.
pub const F_VERSION_1: u64 = 1 << 32;
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;
const DESC_INDIRECT: u16 = 4;
/// Device status bits.
pub const STATUS_DRIVER_OK: u32 = 4;
/// FEATURES_OK.
pub const STATUS_FEATURES_OK: u32 = 8;
/// FAILED.
pub const STATUS_FAILED: u32 = 128;
/// Interrupt status: a used buffer.
pub const ISR_QUEUE: u32 = 1;
/// Interrupt status: the configuration changed.
pub const ISR_CONFIG: u32 = 2;

/// What a device is (the static part of C's `xh_virtio_ops`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// The virtio device ID.
    pub device_id: u32,
    /// Device features besides VERSION_1.
    pub features: u64,
    /// Its queue count (at most [`QUEUES`]).
    pub nqueues: u32,
    /// Queues (a bit per index) the transport does not pop on a kick: the device takes their buffers itself with
    /// [`Virtio::pop`] when it has data (device-to-driver queues). [`Device::kick`] says buffers came.
    pub manual: u32,
}

/// A device model's callbacks. Each runs with the device's lock held.
pub trait Device: Send + Sized + 'static {
    /// A buffer chain arrived on `queue`. Returns the bytes written into its writable parts: the transport puts the
    /// chain on the used ring and raises the interrupt. `None` keeps the chain (completed later with
    /// [`Virtio::push`]).
    fn handle(&self, dev: &Virtio<Self>, queue: u32, head: u16, iov: &[Viov]) -> Option<u32> {
        let _ = (dev, queue, head, iov);
        Some(0)
    }
    /// A read of the device configuration at `offset` (from 0x100).
    fn config_read(&self, dev: &Virtio<Self>, offset: u32, size: u32) -> u32 {
        let _ = (dev, offset, size);
        0
    }
    /// A write of the device configuration.
    fn config_write(&self, dev: &Virtio<Self>, offset: u32, size: u32, value: u32) {
        let _ = (dev, offset, size, value);
    }
    /// The driver reset the device (status 0); the transport's state is cleared already.
    fn reset(&self, dev: &Virtio<Self>) {
        let _ = dev;
    }
    /// The guest posted buffers on a [`Info::manual`] queue.
    fn kick(&self, dev: &Virtio<Self>, queue: u32) {
        let _ = (dev, queue);
    }
    /// Queues (a bit per index) whose driver's VIRTQ_AVAIL_F_NO_INTERRUPT the transport honours: a chain returned there
    /// while the driver's avail ring has the flag sets no interrupt status and raises nothing. None by default (every
    /// return raises the line, whatever the flag: the drivers of the other devices rely on it). The media device
    /// (2026-11-25): its guest polls the command queue, answered inside the notify, and waits on the event queue's.
    fn quiet_queues(&self) -> u32 {
        0
    }
}

// ---- a part of a chain ----------------------------------------------------------------------------------------------

/// One part of a buffer chain (`xh_viov`): guest memory the descriptor names, mapped in this process.
///
/// It stays valid as long as guest RAM is mapped (the process's life in xnuhost). The guest's driver does not touch a
/// buffer while the device holds it (virtio's contract); the safe copies here never make a Rust reference to it.
#[derive(Clone, Copy, Debug)]
pub struct Viov {
    ptr: *mut u8,
    len: u32,
    writable: bool,
}

// SAFETY: a Viov is an address in guest RAM, which every thread of the process may use; it owns nothing.
unsafe impl Send for Viov {}
// SAFETY: as above; its own fields are never changed through a shared reference.
unsafe impl Sync for Viov {}

impl Default for Viov {
    fn default() -> Viov {
        Viov::EMPTY
    }
}

impl Viov {
    /// No memory (what a pop fills).
    pub const EMPTY: Viov = Viov { ptr: std::ptr::null_mut(), len: 0, writable: false };

    /// A part over host memory: for tests and tools that make chains themselves.
    ///
    /// # Safety
    /// `ptr` must be valid for reads (and writes, if `writable`) of `len` bytes for as long as the Viov is used.
    pub unsafe fn from_raw(ptr: *mut u8, len: u32, writable: bool) -> Viov {
        Viov { ptr, len, writable }
    }

    /// Its length in bytes.
    pub fn len(&self) -> u32 {
        self.len
    }

    /// Whether it is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether the device may write it (VIRTQ_DESC_F_WRITE).
    pub fn writable(&self) -> bool {
        self.writable
    }

    /// Its host address (null when empty), for a system call that reads or fills it (pread, ioctl).
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    fn span(&self, offset: usize, want: usize) -> usize {
        (self.len as usize).saturating_sub(offset).min(want)
    }

    /// Copies from `offset` into `out`; returns the bytes copied.
    pub fn read_at(&self, offset: usize, out: &mut [u8]) -> usize {
        let n = self.span(offset, out.len());
        if n > 0 {
            // SAFETY: [offset, offset + n) lies inside the part, which is mapped guest memory (Viov's contract).
            unsafe { std::ptr::copy_nonoverlapping(self.ptr.add(offset), out.as_mut_ptr(), n) };
        }
        n
    }

    /// Copies `data` in at `offset` (a part the guest gave as readable only is not written); returns the bytes copied.
    pub fn write_at(&self, offset: usize, data: &[u8]) -> usize {
        if !self.writable {
            return 0;
        }
        let n = self.span(offset, data.len());
        if n > 0 {
            // SAFETY: as in read_at; the part is device-writable.
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.ptr.add(offset), n) };
        }
        n
    }

    /// Sets `len` bytes from `offset` to `byte` (a writable part only); returns the bytes set.
    pub fn fill(&self, offset: usize, byte: u8, len: usize) -> usize {
        if !self.writable {
            return 0;
        }
        let n = self.span(offset, len);
        if n > 0 {
            // SAFETY: as in write_at.
            unsafe { std::ptr::write_bytes(self.ptr.add(offset), byte, n) };
        }
        n
    }

    /// The part as a slice.
    ///
    /// # Safety
    /// Nothing may write the part while the slice lives (the guest keeps off a posted buffer by virtio's contract).
    pub unsafe fn as_slice(&self) -> &[u8] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: mapped guest memory of this length (Viov's contract); no writer per the caller.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len as usize) }
    }

    /// The part as a mutable slice.
    ///
    /// # Safety
    /// The part must be writable, and nothing else may read or write it while the slice lives.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn as_mut_slice(&self) -> &mut [u8] {
        if self.len == 0 {
            return &mut [];
        }
        // SAFETY: as in as_slice, and exclusive per the caller.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len as usize) }
    }
}

/// The bytes of a chain's readable parts, in order.
pub fn readable_len(iov: &[Viov]) -> usize {
    iov.iter().filter(|v| !v.writable).map(|v| v.len as usize).sum()
}

/// The bytes of a chain's writable parts.
pub fn writable_len(iov: &[Viov]) -> usize {
    iov.iter().filter(|v| v.writable).map(|v| v.len as usize).sum()
}

/// Copies the chain's readable parts, as one stream from byte `skip`, into `out`; returns the bytes copied.
pub fn read_chain(iov: &[Viov], mut skip: usize, out: &mut [u8]) -> usize {
    let mut done = 0;
    for part in iov.iter().filter(|v| !v.writable) {
        let len = part.len as usize;
        if skip >= len {
            skip -= len;
            continue;
        }
        done += part.read_at(skip, &mut out[done..]);
        skip = 0;
        if done == out.len() {
            break;
        }
    }
    done
}

/// Copies `data` into the chain's writable parts, as one stream from byte `skip`; returns the bytes copied.
pub fn write_chain(iov: &[Viov], mut skip: usize, data: &[u8]) -> usize {
    let mut done = 0;
    for part in iov.iter().filter(|v| v.writable) {
        let len = part.len as usize;
        if skip >= len {
            skip -= len;
            continue;
        }
        done += part.write_at(skip, &data[done..]);
        skip = 0;
        if done == data.len() {
            break;
        }
    }
    done
}

// ---- the recursive lock ---------------------------------------------------------------------------------------------

/// A pthread recursive mutex (boxed: it must not move once made).
struct RecursiveMutex(Box<UnsafeCell<libc::pthread_mutex_t>>);

impl RecursiveMutex {
    fn new() -> RecursiveMutex {
        // SAFETY: a zeroed pthread_mutex_t is only storage; pthread_mutex_init makes it a mutex in place (boxed, it
        // never moves). The attribute object lives across the init and is destroyed after.
        unsafe {
            let m = Box::new(UnsafeCell::new(std::mem::zeroed::<libc::pthread_mutex_t>()));
            let mut attr = std::mem::zeroed::<libc::pthread_mutexattr_t>();
            libc::pthread_mutexattr_init(&mut attr);
            libc::pthread_mutexattr_settype(&mut attr, libc::PTHREAD_MUTEX_RECURSIVE);
            libc::pthread_mutex_init(m.get(), &attr);
            libc::pthread_mutexattr_destroy(&mut attr);
            RecursiveMutex(m)
        }
    }

    fn lock(&self) -> RawGuard<'_> {
        // SAFETY: an initialized mutex; a recursive one may be locked again by its holder.
        unsafe { libc::pthread_mutex_lock(self.0.get()) };
        RawGuard(self, std::marker::PhantomData)
    }

    /// The lock if nobody else holds it (its holder may take it again), `None` without waiting.
    fn try_lock(&self) -> Option<RawGuard<'_>> {
        // SAFETY: an initialized mutex; trylock never blocks.
        (unsafe { libc::pthread_mutex_trylock(self.0.get()) } == 0).then(|| RawGuard(self, std::marker::PhantomData))
    }
}

/// Who holds a device now, for the health report, which reads it without the lock when it cannot take it (C's
/// `held_ms`, `held_queue`, `held_cpu`, `held_depth`, `held_said`). Marked by a request's handler (the transport's
/// pop loop on a kick) and by a device thread's [`Virtio::lock`]; a nested mark keeps the outer one. Written only with
/// the device's lock held.
#[derive(Debug)]
struct Holder {
    since_ms: AtomicU64, // CLOCK_MONOTONIC ms; 0: nobody marked
    queue: AtomicI32,    // the queue whose request holds it; -1: a device thread's lock
    cpu: AtomicI32,      // the vCPU that holds it; -1: another thread
    depth: AtomicU32,    // nesting (with the lock held only)
    said: AtomicBool,    // the health report said this hold
}

impl Holder {
    const fn new() -> Holder {
        Holder {
            since_ms: AtomicU64::new(0),
            queue: AtomicI32::new(-1),
            cpu: AtomicI32::new(-1),
            depth: AtomicU32::new(0),
            said: AtomicBool::new(false),
        }
    }

    /// The device's lock held. One clock read per outer mark (the vDSO's, no system call).
    fn hold(&self, queue: i32) {
        if self.depth.fetch_add(1, Ordering::Relaxed) != 0 {
            return;
        }
        self.said.store(false, Ordering::Release);
        self.queue.store(queue, Ordering::Relaxed);
        self.cpu.store(hooks::cpu().map_or(-1, |c| c as i32), Ordering::Relaxed);
        self.since_ms.store(crate::time::monotonic_ms(), Ordering::Release);
    }

    /// The device's lock held; says "free again" when the report said the hold.
    fn release(&self, label: &str) {
        let depth = self.depth.load(Ordering::Relaxed);
        if depth == 0 {
            return;
        }
        self.depth.store(depth - 1, Ordering::Relaxed);
        if depth != 1 {
            return;
        }
        let since = self.since_ms.swap(0, Ordering::AcqRel);
        if self.said.swap(false, Ordering::AcqRel) {
            say(format_args!(
                "virtio {label}: free again: the request that held it ended after {} s",
                crate::time::monotonic_ms().saturating_sub(since) / 1000
            ));
        }
    }

    /// The report could not take the lock at `now_ms`: the line for a hold of `held_ms` or longer, once per hold.
    fn held_line(&self, label: &str, now_ms: u64, held_ms: u64) -> Option<String> {
        let since = self.since_ms.load(Ordering::Acquire);
        if since == 0 || now_ms < since + held_ms || self.said.swap(true, Ordering::AcqRel) {
            return None;
        }
        let (cpu, queue) = (self.cpu.load(Ordering::Relaxed), self.queue.load(Ordering::Relaxed));
        let who = if cpu >= 0 { format!("cpu{cpu}") } else { "a device thread".to_owned() };
        let secs = (now_ms - since) / 1000;
        Some(if queue >= 0 {
            format!(
                "virtio {label}: held by a request on queue {queue} for {secs} s ({who} in its handler): the guest's next access to the device waits for it"
            )
        } else {
            format!("virtio {label}: held by {who} for {secs} s: the guest's next access to the device waits for it")
        })
    }
}

impl Drop for RecursiveMutex {
    fn drop(&mut self) {
        // SAFETY: an initialized mutex nobody holds (we own it exclusively here).
        unsafe { libc::pthread_mutex_destroy(self.0.get()) };
    }
}

/// Holds the lock once; unlocks on drop, on the thread that locked (not Send).
struct RawGuard<'a>(&'a RecursiveMutex, std::marker::PhantomData<*const ()>);

impl Drop for RawGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: this thread locked the mutex when the guard was made.
        unsafe { libc::pthread_mutex_unlock((self.0).0.get()) };
    }
}

/// The device's lock held ([`Virtio::lock`]): the device's state, shared. While it lives the device is marked held by
/// this thread (the health report's "held by a device thread" line).
pub struct Guard<'a, D> {
    virtio: &'a Virtio<D>,
    _raw: RawGuard<'a>,
}

impl<D> std::ops::Deref for Guard<'_, D> {
    type Target = D;
    fn deref(&self) -> &D {
        &self.virtio.device
    }
}

impl<D> Drop for Guard<'_, D> {
    fn drop(&mut self) {
        self.virtio.holder.release(&self.virtio.label); // before the unlock (`_raw` drops after this)
    }
}

// ---- the transport --------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default)]
struct Queue {
    num: u32,
    ready: bool,
    desc: u64,
    avail: u64,
    used: u64,
    last_avail: u16,
}

/// A queue's counters: kept over a reset (the reset clears the queue, not these).
#[derive(Clone, Copy, Debug, Default)]
struct Stats {
    taken: u64,
    returned: u64,
    bad: u64,
    shown_taken: u64,
    shown_returned: u64, // at the last report
    quiet_since_ms: u64, // when the held (or unread) episode began, 0: none
    held_told: bool,
    unread_told: bool,
}

#[derive(Debug, Default)]
struct State {
    status: u32,
    isr: u32,
    features_sel: u32,
    driver_sel: u32,
    queue_sel: u32,
    generation: u32,
    driver_features: u64,
    q: [Queue; QUEUES],
    st: [Stats; QUEUES],
}

impl State {
    fn selected(&mut self) -> &mut Queue {
        let i = if (self.queue_sel as usize) < QUEUES { self.queue_sel as usize } else { 0 };
        &mut self.q[i]
    }
}

/// A virtio-mmio device: the transport's state and the device model `D` (`xh_virtio`).
pub struct Virtio<D> {
    lock: RecursiveMutex,
    // Borrowed only with `lock` held, and never across a call into the device or out of this module.
    state: RefCell<State>,
    info: Info,
    name: String,
    label: String,
    spi: u32,
    holder: Holder,
    bad_seen: AtomicU64, // its bad chains at the last report that took its lock (a held device's part of the next)
    device: D,
}

// SAFETY: `state` and `device` are reached only with the recursive lock held, so one thread at a time; the device
// moves between threads with it (D: Send). The pthread mutex itself is thread-safe.
unsafe impl<D: Send> Sync for Virtio<D> {}
// SAFETY: as above; nothing in Virtio is tied to the thread that made it.
unsafe impl<D: Send> Send for Virtio<D> {}

static LOG_ON: AtomicBool = AtomicBool::new(false);

/// Whether the transport's lines (the driver's state changes, [`tick`]'s report) go to the log (`xh_virtio_log`):
/// xnuhost turns it on; off by default (the bridges' tools and tests).
pub fn set_log(on: bool) {
    LOG_ON.store(on, Ordering::Relaxed);
}

fn say(args: std::fmt::Arguments<'_>) {
    #[cfg(test)]
    tests::said(args);
    if LOG_ON.load(Ordering::Relaxed) {
        crate::log::write(crate::log::Level::Info, args);
    }
}

fn translate(pa: u64, len: u64) -> Option<NonNull<u8>> {
    hooks::ram(pa, len)
}

/// The guest's ring index at `p` (a u16 the guest's driver writes, read with acquire as C did).
///
/// # Safety
/// `p` must point at a mapped u16 of guest RAM.
unsafe fn ring_index(p: *mut u16) -> Option<&'static AtomicU16> {
    if (p as usize) % 2 != 0 {
        return None; // a misaligned ring: not usable
    }
    // SAFETY: an aligned, mapped u16 of guest RAM (the caller's contract), shared with the guest as an atomic.
    Some(unsafe { AtomicU16::from_ptr(p) })
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

impl<D: Device> Virtio<D> {
    /// Makes a device (`xh_virtio_create`) on the machine's [`Registry`] and, unless `base` is 0, puts its 4 KiB of
    /// registers on the MMIO bus at `base`. `name` is the device tree's ("virtio-gpu"); the health lines name it
    /// "gpu#0" (the name without `virtio-`, its index among the devices of that name).
    pub fn create(base: u64, spi: u32, info: Info, device: D, name: &str) -> Result<Arc<Virtio<D>>, AddError> {
        Virtio::create_in(Registry::global(), base, spi, info, device, name)
    }

    /// [`create`](Self::create) on a given registry (a test's own).
    pub fn create_in(
        registry: &Registry,
        base: u64,
        spi: u32,
        info: Info,
        device: D,
        name: &str,
    ) -> Result<Arc<Virtio<D>>, AddError> {
        let mut reg = registry.inner();
        let index = reg.count.get(name).copied().unwrap_or(0);
        let kind = name.strip_prefix("virtio-").unwrap_or(if name.is_empty() { "?" } else { name });
        let mut label = format!("{kind}#{index}");
        crate::text::truncate_string(&mut label, 23); // C's char label[24]
        let dev = Arc::new(Virtio {
            lock: RecursiveMutex::new(),
            state: RefCell::new(State::default()),
            info,
            name: name.to_owned(),
            label,
            spi,
            holder: Holder::new(),
            bad_seen: AtomicU64::new(0),
            device,
        });
        if base != 0 {
            mmio::bus().add(base, 0x1000, name, dev.clone())?;
        }
        *reg.count.entry(name.to_owned()).or_default() += 1;
        let weak: Weak<dyn Report> = Arc::downgrade(&(dev.clone() as Arc<dyn Report>));
        reg.devices.push(weak);
        Ok(dev)
    }
}

impl<D: Device> Virtio<D> {
    /// The device's name ("virtio-gpu").
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The health lines' name ("gpu#0").
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Its interrupt (INTID).
    pub fn spi(&self) -> u32 {
        self.spi
    }

    /// What the device is.
    pub fn info(&self) -> &Info {
        &self.info
    }

    /// Takes the device's lock (recursive) and gives its state.
    pub fn lock(&self) -> Guard<'_, D> {
        let raw = self.lock.lock();
        self.holder.hold(-1);
        Guard { virtio: self, _raw: raw }
    }

    /// The device's state without the lock, for a state that is itself thread-safe (atomics).
    pub fn device(&self) -> &D
    where
        D: Sync,
    {
        &self.device
    }

    fn st(&self) -> std::cell::RefMut<'_, State> {
        self.state.borrow_mut()
    }

    fn raise(&self, st: &mut State, bits: u32) {
        st.isr |= bits;
        hooks::set_spi(self.spi, true);
    }

    /// Reads the chain starting at `head` into `iov`. Returns the count of parts, 0 for a bad chain.
    fn chain(q: &Queue, head: u16, iov: &mut [Viov]) -> usize {
        let Some(mut table) = translate(q.desc, 16 * u64::from(q.num)) else { return 0 };
        let (mut n, mut seen, mut count, mut indirect) = (0usize, 0u32, q.num, false);
        let mut i = head;
        loop {
            seen += 1;
            if u32::from(i) >= count || seen > count {
                return 0;
            }
            // SAFETY: `table` maps `count` descriptors of 16 bytes (translated with that length) and i < count.
            let e: Desc = unsafe { table.as_ptr().cast::<Desc>().add(usize::from(i)).read_unaligned() };
            if e.flags & DESC_INDIRECT != 0 {
                if indirect || e.len % 16 != 0 || e.len == 0 {
                    return 0; // one level only
                }
                indirect = true;
                count = e.len / 16;
                let Some(t) = translate(e.addr, u64::from(e.len)) else { return 0 };
                table = t;
                i = 0;
                seen = 0;
                continue;
            }
            if n == iov.len() {
                return 0;
            }
            let ptr = if e.len != 0 {
                match translate(e.addr, u64::from(e.len)) {
                    Some(p) => p.as_ptr(),
                    None => return 0,
                }
            } else {
                std::ptr::null_mut()
            };
            iov[n] = Viov { ptr, len: e.len, writable: e.flags & DESC_WRITE != 0 };
            n += 1;
            if e.flags & DESC_NEXT == 0 {
                break;
            }
            i = e.next;
        }
        n
    }

    /// Whether the driver set VIRTQ_AVAIL_F_NO_INTERRUPT (bit 0 of its avail ring's flags); false for a ring not in RAM.
    fn no_interrupt(q: &Queue) -> bool {
        let Some(avail) = translate(q.avail, 2) else { return false };
        // SAFETY: the ring's first two bytes (its flags) are mapped; read as the guest's u16, with acquire.
        unsafe { ring_index(avail.as_ptr().cast()) }.is_some_and(|f| f.load(Ordering::Acquire) & 1 != 0)
    }

    /// The guest's avail index of a queue, `None` if its ring is not in RAM.
    fn avail_idx(q: &Queue) -> Option<(u16, NonNull<u8>)> {
        let avail = translate(q.avail, 4 + 2 * u64::from(q.num))?;
        // SAFETY: the ring's 4 + 2 * num bytes are mapped; its index is the u16 at byte 2.
        let idx = unsafe { ring_index(avail.as_ptr().add(2).cast()) }?;
        Some((idx.load(Ordering::Acquire), avail))
    }

    /// Takes the next available chain: `Some((head, parts))`, parts 0 for a bad chain; `None` when none is posted.
    fn take(st: &mut State, qi: usize, iov: &mut [Viov]) -> Option<(u16, usize)> {
        let q = st.q[qi];
        if !q.ready || q.num == 0 {
            return None;
        }
        let (idx, avail) = Self::avail_idx(&q)?;
        if idx == q.last_avail {
            return None;
        }
        let slot = usize::from(q.last_avail) % q.num as usize;
        // SAFETY: slot < num, so 4 + 2 * slot + 2 <= 4 + 2 * num: inside the translated ring.
        let head = unsafe { avail.as_ptr().add(4 + 2 * slot).cast::<u16>().read_unaligned() };
        st.q[qi].last_avail = q.last_avail.wrapping_add(1);
        let n = Self::chain(&q, head, iov);
        st.st[qi].taken += 1;
        if n == 0 {
            st.st[qi].bad += 1;
        }
        Some((head, n))
    }

    fn put(&self, st: &mut State, qi: usize, head: u16, written: u32) {
        let q = st.q[qi];
        if q.num == 0 {
            return;
        }
        let Some(used) = translate(q.used, 4 + 8 * u64::from(q.num)) else { return };
        let base = used.as_ptr();
        // SAFETY: the used ring's 4 + 8 * num bytes are mapped; the index is the u16 at byte 2 and slot < num.
        unsafe {
            let Some(idx) = ring_index(base.add(2).cast()) else { return };
            let at = idx.load(Ordering::Relaxed);
            let slot = usize::from(at) % q.num as usize;
            let elem = base.add(4 + 8 * slot).cast::<u32>();
            elem.write_unaligned(u32::from(head));
            elem.add(1).write_unaligned(written);
            idx.store(at.wrapping_add(1), Ordering::Release);
        }
        st.st[qi].returned += 1;
        if self.device.quiet_queues() & (1 << qi) != 0 && Self::no_interrupt(&q) {
            return; // the driver asked for none on this queue (VIRTQ_AVAIL_F_NO_INTERRUPT)
        }
        self.raise(st, ISR_QUEUE);
    }

    /// Takes the next available chain of `queue` without the guest's kick (device-initiated data, as input events):
    /// `Some((head, parts))`, or `None` when the guest posted none or its driver is not up. A bad chain is returned
    /// unused and gives `None` (`xh_virtio_pop`).
    pub fn pop(&self, queue: u32, iov: &mut [Viov]) -> Option<(u16, usize)> {
        let _g = self.lock.lock();
        let mut st = self.st();
        let qi = queue as usize;
        if qi >= QUEUES || st.status & STATUS_DRIVER_OK == 0 {
            return None;
        }
        let (head, n) = Self::take(&mut st, qi, iov)?;
        if n == 0 {
            self.put(&mut st, qi, head, 0); // a bad chain is returned unused
            return None;
        }
        Some((head, n))
    }

    /// Returns chain `head` to the guest with `written` bytes in it and raises the interrupt (`xh_virtio_push`).
    pub fn push(&self, queue: u32, head: u16, written: u32) {
        let _g = self.lock.lock();
        if (queue as usize) < QUEUES {
            self.put(&mut self.st(), queue as usize, head, written);
        }
    }

    /// The features the guest's driver accepted (bits 0-63).
    pub fn driver_features(&self) -> u64 {
        let _g = self.lock.lock();
        self.st().driver_features
    }

    /// Whether the guest's driver has set DRIVER_OK: the device's queues are in use.
    pub fn ready(&self) -> bool {
        let _g = self.lock.lock();
        self.st().status & STATUS_DRIVER_OK != 0
    }

    /// Tells the driver the configuration changed: a new generation and the config interrupt.
    pub fn config_changed(&self) {
        let _g = self.lock.lock();
        let mut st = self.st();
        st.generation = st.generation.wrapping_add(1);
        self.raise(&mut st, ISR_CONFIG);
    }

    fn reset(&self) {
        {
            let mut st = self.st();
            if st.status & STATUS_DRIVER_OK != 0 {
                say(format_args!("virtio {}: reset by the guest's driver (it was up)", self.label));
            }
            for s in st.st.iter_mut() {
                // what was in flight is the reset's: not held any more
                *s = Stats { taken: s.returned, shown_taken: s.returned, shown_returned: s.returned, ..*s };
                s.quiet_since_ms = 0;
                s.held_told = false;
                s.unread_told = false;
            }
            let generation = st.generation;
            let stats = st.st;
            *st = State { generation, st: stats, ..State::default() };
        }
        hooks::set_spi(self.spi, false);
        self.device.reset(self);
    }

    fn notify(&self, queue: u32) {
        let qi = queue as usize;
        {
            let st = self.st();
            if queue >= self.info.nqueues || qi >= QUEUES || st.status & STATUS_DRIVER_OK == 0 {
                return;
            }
        }
        if self.info.manual & (1 << queue) != 0 {
            self.device.kick(self, queue);
            return;
        }
        let mut iov = [Viov::EMPTY; MAX_PARTS];
        self.holder.hold(queue as i32); // the request's handler holds the device (the health report's "held by")
        loop {
            let Some((head, n)) = Self::take(&mut self.st(), qi, &mut iov) else { break };
            let written = if n > 0 { self.device.handle(self, queue, head, &iov[..n]) } else { Some(0) };
            if let Some(written) = written {
                self.put(&mut self.st(), qi, head, written);
            }
        }
        self.holder.release(&self.label);
    }

    /// A read of the device's registers at `offset` (`xh_virtio_mmio_read`).
    pub fn mmio_read(&self, offset: u64, size: u32) -> u64 {
        let _g = self.lock.lock();
        if offset >= 0x100 {
            return u64::from(self.device.config_read(self, (offset - 0x100) as u32, size));
        }
        let mut st = self.st();
        let info = &self.info;
        let v = match offset {
            0x000 => MAGIC,
            0x004 => 2,
            0x008 => info.device_id,
            0x00c => VENDOR,
            0x010 => match st.features_sel {
                0 => info.features as u32,
                1 => ((info.features | F_VERSION_1) >> 32) as u32,
                _ => 0,
            },
            0x034 => {
                if st.queue_sel < info.nqueues {
                    QSIZE
                } else {
                    0
                }
            }
            0x044 => st.selected().ready.into(),
            0x060 => st.isr,
            0x070 => st.status,
            0x0fc => st.generation,
            _ => 0,
        };
        u64::from(v)
    }

    /// A write of the device's registers at `offset` (`xh_virtio_mmio_write`).
    pub fn mmio_write(&self, offset: u64, size: u32, value: u64) {
        let _g = self.lock.lock();
        let v = value as u32;
        if offset >= 0x100 {
            self.device.config_write(self, (offset - 0x100) as u32, size, v);
            return;
        }
        let mut st = self.st();
        match offset {
            0x014 => st.features_sel = v,
            0x020 => {
                if st.driver_sel < 2 {
                    let high = st.driver_sel == 1;
                    set_half(&mut st.driver_features, high, v);
                }
            }
            0x024 => st.driver_sel = v,
            0x030 => st.queue_sel = v,
            0x038 => {
                if v != 0 && v <= QSIZE && v.is_power_of_two() {
                    st.selected().num = v;
                }
            }
            0x044 => {
                let q = st.selected();
                q.ready = v & 1 != 0;
                if !q.ready {
                    q.last_avail = 0;
                }
            }
            0x050 => {
                drop(st);
                self.notify(v);
            }
            0x064 => {
                st.isr &= !v;
                if st.isr == 0 {
                    hooks::set_spi(self.spi, false);
                }
            }
            0x070 => {
                if v == 0 {
                    drop(st);
                    self.reset();
                } else {
                    self.set_status(&mut st, v);
                }
            }
            0x080 => set_half(&mut st.selected().desc, false, v),
            0x084 => set_half(&mut st.selected().desc, true, v),
            0x090 => set_half(&mut st.selected().avail, false, v),
            0x094 => set_half(&mut st.selected().avail, true, v),
            0x0a0 => set_half(&mut st.selected().used, false, v),
            0x0a4 => set_half(&mut st.selected().used, true, v),
            _ => {}
        }
    }

    fn set_status(&self, st: &mut State, mut v: u32) {
        // FEATURES_OK is refused unless the driver accepted VERSION_1 and nothing the device lacks.
        let offered = self.info.features | F_VERSION_1;
        if v & STATUS_FEATURES_OK != 0 && (st.driver_features & F_VERSION_1 == 0 || st.driver_features & !offered != 0)
        {
            v &= !STATUS_FEATURES_OK;
        }
        if v & STATUS_DRIVER_OK != 0 && st.status & STATUS_DRIVER_OK == 0 {
            say(format_args!(
                "virtio {}: the guest's driver is up (features 0x{:x}, {} queue(s) ready)",
                self.label,
                st.driver_features,
                st.q.iter().filter(|q| q.ready).count()
            ));
        }
        if v & STATUS_FAILED != 0 && st.status & STATUS_FAILED == 0 {
            say(format_args!("virtio {}: the guest's driver set FAILED (status 0x{:x})", self.label, v));
        }
        st.status = v;
    }
}

fn set_half(value: &mut u64, high: bool, half: u32) {
    *value =
        if high { (*value & 0xffff_ffff) | u64::from(half) << 32 } else { (*value & !0xffff_ffff) | u64::from(half) };
}

impl<D: Device> MmioDevice for Virtio<D> {
    fn mmio_read(&self, offset: u64, size: u32) -> u64 {
        Virtio::mmio_read(self, offset, size)
    }
    fn mmio_write(&self, offset: u64, size: u32, value: u64) {
        Virtio::mmio_write(self, offset, size, value)
    }
}

// ---- health ---------------------------------------------------------------------------------------------------------

/// The summary's room (C's `char sum[900]`): a part that does not fit is left out.
const SUM_ROOM: usize = 899;

/// A device as the health report sees it.
trait Report: Send + Sync {
    /// Adds the device's part to the summary `sum`, says its episode lines, and returns its bad chains (all queues).
    /// Never waits for the device's lock: a device held by another thread says so once per hold of `held_ms` or longer
    /// and gives its bad chains of the last report; its counts wait for the next.
    fn report(&self, now_ms: u64, held_ms: u64, sum: &mut String, out: &mut dyn FnMut(&str)) -> u64;
}

fn append(sum: &mut String, part: std::fmt::Arguments<'_>) {
    let mut s = String::new();
    let _ = s.write_fmt(part);
    if sum.len() + s.len() <= SUM_ROOM {
        sum.push_str(&s);
    }
}

impl<D: Device> Report for Virtio<D> {
    fn report(&self, now_ms: u64, held_ms: u64, sum: &mut String, out: &mut dyn FnMut(&str)) -> u64 {
        // Never waits for a device: a handler that does not come back held this report, and with it xnuhost's main
        // loop and the session's end (phone, 2026-11-23: the encoder's open waited for ever on cpu1 with the
        // virtio-media device held; the main loop stopped in this lock 78 s before the guest's panic, and the halt was
        // never seen). A device held longer than `held_ms` is said once per hold; its counts wait for the next report.
        let Some(_g) = self.lock.try_lock() else {
            if let Some(mut line) = self.holder.held_line(&self.label, now_ms, held_ms) {
                crate::text::truncate_string(&mut line, 191); // C's char line[192]
                out(&line);
            }
            // its bad chains as last seen (C left them out: that "bad chains +N" wrapped, the next said them again)
            return self.bad_seen.load(Ordering::Relaxed);
        };
        let mut st = self.st();
        let mut named = false;
        for i in 0..(self.info.nqueues as usize).min(QUEUES) {
            let q = st.q[i];
            let s = &mut st.st[i];
            let returned = s.returned - s.shown_returned;
            let taken = s.taken - s.shown_taken;
            let in_flight = s.taken.saturating_sub(s.returned);
            // Chains the guest posted that nobody took: only a queue the transport pops on the guest's kick can have
            // them waiting for no reason (a device's own queue keeps the guest's buffers until it has data).
            let mut unread = 0u16;
            if q.ready && q.num != 0 && self.info.manual & (1 << i) == 0 {
                if let Some((idx, _)) = Self::avail_idx(&q) {
                    unread = idx.wrapping_sub(q.last_avail);
                }
            }
            if returned != 0 || taken != 0 {
                let sep = if sum.is_empty() || named { "" } else { ", " };
                let label = if named { "" } else { self.label.as_str() };
                append(sum, format_args!("{sep}{label} q{i} +{returned}"));
                if in_flight != 0 {
                    append(sum, format_args!(" (in flight {in_flight})"));
                }
                named = true;
            }
            // An episode: chains held (or left unread) and nothing returned for a whole span. Told once; and when it
            // moves again.
            let quiet = returned == 0 && (in_flight != 0 || unread != 0);
            let label = &self.label;
            if quiet {
                if s.quiet_since_ms == 0 {
                    s.quiet_since_ms = now_ms;
                } else if !s.held_told && in_flight != 0 {
                    out(&format!(
                        "virtio {label}: queue {i}: {in_flight} chain(s) in flight and none returned in {} s (taken {}, returned {} since the boot)",
                        (now_ms - s.quiet_since_ms) / 1000,
                        s.taken,
                        s.returned
                    ));
                    s.held_told = true;
                } else if !s.unread_told && unread != 0 && in_flight == 0 {
                    out(&format!(
                        "virtio {label}: queue {i}: {unread} chain(s) posted by the guest and not taken in {} s",
                        (now_ms - s.quiet_since_ms) / 1000
                    ));
                    s.unread_told = true;
                }
            } else {
                if s.held_told || s.unread_told {
                    out(&format!(
                        "virtio {label}: queue {i} returns chains again (after {} s)",
                        now_ms.wrapping_sub(s.quiet_since_ms) / 1000
                    ));
                }
                s.quiet_since_ms = 0;
                s.held_told = false;
                s.unread_told = false;
            }
            s.shown_returned = s.returned;
            s.shown_taken = s.taken;
        }
        let bad = st.st.iter().map(|s| s.bad).sum();
        self.bad_seen.store(bad, Ordering::Relaxed);
        bad
    }
}

#[derive(Default)]
struct RegistryInner {
    devices: Vec<Weak<dyn Report>>,
    count: HashMap<String, u32>, // devices made of each name: the label's index
    last_ms: u64,
    bad_shown: u64,
    next_ms: u64,
    held_ms: Option<u64>, // None: HELD_MS
}

/// The devices the health report covers. The machine's is [`Registry::global`]; a test may make its own.
#[derive(Default)]
pub struct Registry {
    inner: Mutex<RegistryInner>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Registry({} devices)", self.inner().devices.len())
    }
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Registry {
        Registry::default()
    }

    /// The machine's registry ([`Virtio::create`]'s).
    pub fn global() -> &'static Registry {
        static GLOBAL: OnceLock<Registry> = OnceLock::new();
        GLOBAL.get_or_init(Registry::new)
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, RegistryInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The hold the report says (`xh_virtio_held_ms`, [`HELD_MS`] unless set): a test's registry sets less.
    pub fn set_held_ms(&self, ms: u64) {
        self.inner().held_ms = Some(ms);
    }

    /// The queues' health at `now_ms` (`xh_virtio_health`), each line to `out`, only when something changed:
    /// `virtio: 60 s: gpu#0 q0 +1200 q1 +30, input#0 q0 +5400, blk#3 q0 +544 (in flight 1)` (+N: chains returned to
    /// the guest in the span), and once per episode a queue that holds chains with none returned for a whole span
    /// (`virtio gpu#0: queue 0: 2 chain(s) in flight and none returned in 60 s ...`), a queue whose posted chains
    /// nobody took, and the line that says it moves again. It never waits for a device's lock: a device held for the
    /// held span ([`HELD_MS`]) or longer by one request (or a device thread's [`Virtio::lock`]) is said once per hold,
    /// with the vCPU in its handler (`virtio media#0: held by a request on queue 0 for 78 s (cpu1 in its handler)`),
    /// and again when it is free.
    pub fn health(&self, now_ms: u64, out: &mut dyn FnMut(&str)) {
        let mut reg = self.inner();
        reg.devices.retain(|d| d.strong_count() > 0);
        let mut sum = String::new();
        let mut bad = 0;
        let held_ms = reg.held_ms.unwrap_or(HELD_MS);
        for dev in reg.devices.iter().filter_map(Weak::upgrade) {
            bad += dev.report(now_ms, held_ms, &mut sum, out);
        }
        if !sum.is_empty() {
            let bads = if bad != reg.bad_shown {
                format!("; bad chains +{}", bad.wrapping_sub(reg.bad_shown))
            } else {
                String::new()
            };
            let span = if reg.last_ms != 0 && now_ms > reg.last_ms {
                (now_ms - reg.last_ms + 500) / 1000
            } else {
                REPORT_MS / 1000
            };
            let mut line = format!("virtio: {span} s: {sum}{bads}");
            crate::text::truncate_string(&mut line, 1023);
            out(&line);
        }
        reg.bad_shown = bad;
        reg.last_ms = now_ms;
    }

    /// The main loop's call: the first gives the baseline, then a report every [`REPORT_MS`], to the log; nothing
    /// unless [`set_log`] is on.
    pub fn tick(&self, now_ms: u64) {
        if !LOG_ON.load(Ordering::Relaxed) {
            return;
        }
        let mut out = |line: &str| crate::log::write_str(crate::log::Level::Info, line);
        let next = self.inner().next_ms;
        if next != 0 && now_ms < next {
            return;
        }
        self.inner().next_ms = now_ms + REPORT_MS;
        self.health(now_ms, &mut out);
    }
}

/// The main loop's call on the machine's registry (`xh_virtio_tick`), at CLOCK_MONOTONIC's now.
pub fn tick() {
    Registry::global().tick(crate::time::monotonic_ms());
}

pub mod testing;

#[cfg(test)]
mod tests;
