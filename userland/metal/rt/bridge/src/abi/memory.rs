// SPDX-License-Identifier: MIT
//! Buffers and heaps. In rt the consumer takes blocks (64 KiB granules of 16 MiB units) and places buffers in them, so:
//! a buffer of more than 32 KiB takes a block of its own; a smaller one a slot of a slab (a 256 KiB block of slots of one
//! power-of-two size from 256 B), given back through the retire gate (a released slot may still be read by a command buffer
//! made before the release); a no-copy buffer is an import of the caller's pages; a heap is one block and the bridge's own
//! placement in it.

use super::device::{device_of, DeviceRef, NmbDevice};
use super::{boxed, e, get, guard, release, Options};
use metal2vulkan_runtime as rt;
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

/// The largest buffer a slab slot holds.
const SMALL_MAX: u64 = 32 << 10;
const SMALL_MIN: u64 = 256;
const SLAB: u64 = 256 << 10;
const SIZE_CLASSES: usize = 8; // 256 B .. 32 KiB

fn size_class(length: u64) -> usize {
    let size = length.max(SMALL_MIN).next_power_of_two();
    (size.trailing_zeros() - SMALL_MIN.trailing_zeros()) as usize
}

fn class_index(class: rt::MemoryClass) -> usize {
    match class {
        rt::MemoryClass::Private => 1,
        _ => 0,
    }
}

struct Slab {
    block: rt::Block,
    free: Vec<u32>,
    slots: u32,
}

/// The slabs of every (memory class, size class).
#[derive(Default)]
pub struct SmallPools {
    pools: Mutex<[[HashMap<u64, Slab>; SIZE_CLASSES]; 2]>,
}

impl SmallPools {
    fn take(&self, device: &rt::Device, class: rt::MemoryClass, length: u64) -> Result<(u64, u64, u64), String> {
        let sc = size_class(length);
        let size = SMALL_MIN << sc;
        let mut pools = self.pools.lock().unwrap_or_else(|p| p.into_inner());
        let pool = &mut pools[class_index(class)][sc];
        if let Some((&id, slab)) = pool.iter_mut().find(|(_, s)| !s.free.is_empty()) {
            let slot = slab.free.pop().expect("a free slot");
            return Ok((id, slot as u64 * size, size));
        }
        let block = device.take_block(class, SLAB).map_err(e("small buffer slab"))?;
        let slots = (SLAB / size) as u32;
        let id = block.id.get() as u64;
        let mut slab = Slab { block, free: (1..slots).rev().collect(), slots };
        slab.free.shrink_to_fit();
        pool.insert(id, slab);
        Ok((id, 0, size))
    }

    fn block(&self, class: rt::MemoryClass, length: u64, id: u64) -> Option<rt::BlockId> {
        let pools = self.pools.lock().unwrap_or_else(|p| p.into_inner());
        pools[class_index(class)][size_class(length)].get(&id).map(|s| s.block.id)
    }

    /// Gives a slot back; a slab left empty goes back to rt where its pool has another with room.
    fn give(&self, class: rt::MemoryClass, length: u64, id: u64, offset: u64) {
        let sc = size_class(length);
        let size = SMALL_MIN << sc;
        let mut pools = self.pools.lock().unwrap_or_else(|p| p.into_inner());
        let pool = &mut pools[class_index(class)][sc];
        let empty = match pool.get_mut(&id) {
            Some(slab) => {
                slab.free.push((offset / size) as u32);
                slab.free.len() as u32 == slab.slots
            }
            None => false,
        };
        if empty && pool.iter().any(|(&other, s)| other != id && !s.free.is_empty()) {
            pool.remove(&id);
        }
    }

    /// Gives every empty slab back to rt (memory pressure, the plug-in's idle trim): how many went.
    pub fn trim(&self) -> usize {
        let mut pools = self.pools.lock().unwrap_or_else(|p| p.into_inner());
        let mut n = 0;
        for class in pools.iter_mut() {
            for pool in class.iter_mut() {
                let before = pool.len();
                pool.retain(|_, s| s.free.len() as u32 != s.slots);
                n += before - pool.len();
            }
        }
        n
    }
}

/// What a buffer gives back at its release.
pub(crate) enum Hold {
    /// A block of its own (rt gives it back once no command buffer uses it).
    Block(#[allow(dead_code)] rt::Block),
    /// A slab's slot, given back through the retire gate.
    Slot { class: rt::MemoryClass, length: u64, slab: u64, offset: u64 },
    /// A range of a heap the bridge placed (automatic heaps), given back through the gate; `None` for a placement heap's.
    Heap(Arc<HeapInner>, Option<(u64, u64)>),
}

pub struct NmbBuffer {
    pub(crate) buffer: rt::Buffer,
    pub(crate) device: DeviceRef,
    hold: Option<Hold>,
    pub(crate) options: u64,
    purgeable: AtomicU64,
}

impl Drop for NmbBuffer {
    fn drop(&mut self) {
        let d = device_of(self.device);
        match self.hold.take() {
            Some(Hold::Slot { class, length, slab, offset }) => {
                let pools = d.pools.clone();
                d.gate.after_retire(move || pools.give(class, length, slab, offset));
            }
            Some(Hold::Heap(heap, Some(range))) => d.gate.after_retire(move || heap.free(range)),
            _ => {}
        }
    }
}

impl NmbBuffer {
    pub(crate) fn new(buffer: rt::Buffer, device: DeviceRef, hold: Hold, options: u64) -> NmbBuffer {
        NmbBuffer { buffer, device, hold: Some(hold), options, purgeable: AtomicU64::new(2) }
    }
}

/// A buffer of `length` bytes of `options`' class: zeroed, or `bytes` copied in (through Shared memory, which is the GPU's on this
/// unified device, where Private was asked with bytes).
pub(crate) fn new_buffer(d: &NmbDevice, length: u64, options: u64, bytes: Option<&[u8]>) -> Result<NmbBuffer, String> {
    let o = Options(options);
    let class = if bytes.is_some() { rt::MemoryClass::Shared } else { o.class() };
    let length = length.max(1);
    let zero = if bytes.is_some() { None } else { Some(0) };
    let (buffer, hold) = if length <= SMALL_MAX {
        let (slab, offset, _) = d.pools.take(&d.device, class, length)?;
        let block = d.pools.block(class, length, slab).ok_or("a slab gone")?;
        let buffer = d.device.new_buffer(block, offset, length, zero).map_err(e("buffer"))?;
        (buffer, Hold::Slot { class, length, slab, offset })
    } else {
        let block = d.device.take_block(class, length).map_err(e("buffer block"))?;
        let buffer = d.device.new_buffer(block.id, 0, length, zero).map_err(e("buffer"))?;
        (buffer, Hold::Block(block))
    };
    if let Some(bytes) = bytes {
        let contents = buffer.contents().ok_or("a Shared buffer without contents")?;
        // SAFETY: the buffer holds `length` >= bytes.len() bytes, and no command buffer uses it yet.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), contents.as_ptr(), bytes.len().min(length as usize)) };
    }
    Ok(NmbBuffer::new(buffer, d as *const NmbDevice, hold, options))
}

/// # Safety
/// `bytes` is null or holds `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_buffer_new(device: *mut NmbDevice, length: u64, options: u64, bytes: *const c_void) -> *mut NmbBuffer {
    guard(std::ptr::null_mut(), || {
        let d = get(device)?;
        // SAFETY: the caller's contract.
        let bytes = (!bytes.is_null()).then(|| unsafe { std::slice::from_raw_parts(bytes as *const u8, length as usize) });
        Ok(boxed(new_buffer(d, length, options, bytes)?))
    })
}

/// The C side of a no-copy deallocator: `release(context, pointer, length)`.
pub type NmbRelease = unsafe extern "C" fn(*mut c_void, *mut c_void, u64);

struct SendPtr(*mut c_void);
// SAFETY: the caller hands the context over for a call on any thread (Metal's deallocator contract).
unsafe impl Send for SendPtr {}

/// # Safety
/// `pointer` is page aligned, holds `length` bytes and stays mapped until `release` runs (or the buffer is released, without one).
unsafe fn no_copy(device: *mut NmbDevice, pointer: *mut c_void, length: u64, options: u64, release: Option<(NmbRelease, *mut c_void)>)
                  -> *mut NmbBuffer {
    guard(std::ptr::null_mut(), || {
        let d = get(device)?;
        let base = NonNull::new(pointer as *mut u8).ok_or("null memory")?;
        let release = release.map(|(f, context)| {
            let (context, pointer) = (SendPtr(context), SendPtr(pointer));
            Box::new(move || {
                let (context, pointer) = (context, pointer);
                // SAFETY: the caller's contract.
                unsafe { f(context.0, pointer.0, length) }
            }) as Box<dyn FnOnce() + Send>
        });
        // SAFETY: the caller's contract: the pages are mapped readable and writable until the release runs.
        let (block, _) = unsafe { d.device.import_host_memory(base, length, release) }.map_err(e("no-copy buffer"))?;
        let buffer = d.device.new_buffer(block.id, 0, length, None).map_err(e("no-copy buffer"))?;
        Ok(boxed(NmbBuffer::new(buffer, d as *const NmbDevice, Hold::Block(block), options)))
    })
}

/// # Safety
/// `pointer` is page aligned, holds `length` bytes and stays mapped until the buffer is released.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_buffer_new_no_copy(device: *mut NmbDevice, pointer: *mut c_void, length: u64, options: u64) -> *mut NmbBuffer {
    // SAFETY: the caller's contract.
    unsafe { no_copy(device, pointer, length, options, None) }
}

/// `nmb_buffer_new_no_copy` with Metal's deallocator: `release` runs once, on any thread, once rt frees its import of the pages
/// (the last buffer over them and every command buffer using them are gone). A refused call never runs it.
///
/// # Safety
/// `pointer` is page aligned, holds `length` bytes and stays mapped until `release` runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_buffer_new_no_copy_releasing(device: *mut NmbDevice, pointer: *mut c_void, length: u64, options: u64,
                                                          release: Option<NmbRelease>, context: *mut c_void) -> *mut NmbBuffer {
    // SAFETY: the caller's contract.
    unsafe { no_copy(device, pointer, length, options, release.map(|f| (f, context))) }
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_buffer_release(buffer: *mut NmbBuffer) {
    release(buffer)
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_buffer_contents(buffer: *const NmbBuffer) -> *mut c_void {
    guard(std::ptr::null_mut(), || Ok(get(buffer)?.buffer.contents().map_or(std::ptr::null_mut(), |p| p.as_ptr() as *mut c_void)))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_buffer_length(buffer: *const NmbBuffer) -> u64 {
    guard(0, || Ok(get(buffer)?.buffer.length()))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_buffer_gpu_address(buffer: *const NmbBuffer) -> u64 {
    guard(0, || Ok(get(buffer)?.buffer.gpu_address()))
}

/// `setPurgeableState:`: rt makes nothing purgeable, so the state is kept and answered as Metal does (the earlier state;
/// KeepCurrent, 1, asks it).
pub(crate) fn purgeable(cell: &AtomicU64, state: u64) -> u64 {
    if state == 1 { cell.load(Relaxed) } else { cell.swap(state, Relaxed) }
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_buffer_set_purgeable_state(buffer: *const NmbBuffer, state: u64) -> u64 {
    guard(0, || Ok(purgeable(&get(buffer)?.purgeable, state)))
}

// ---- heaps ----

/// A heap's block and the bridge's placement in it.
pub struct HeapInner {
    pub(crate) block: rt::Block,
    pub(crate) size: u64,
    /// Free ranges (start, length), in order.
    free: Mutex<Vec<(u64, u64)>>,
    used: AtomicU64,
}

impl HeapInner {
    pub(crate) fn range(&self) -> rt::HeapRange {
        rt::HeapRange { block: self.block.id, offset: 0, len: self.size }
    }

    /// The first free range that holds `size` bytes at a multiple of `align`.
    pub(crate) fn place(&self, size: u64, align: u64) -> Option<u64> {
        let align = align.max(1);
        let mut free = self.free.lock().unwrap_or_else(|p| p.into_inner());
        for i in 0..free.len() {
            let (start, len) = free[i];
            let at = start.div_ceil(align) * align;
            if at + size <= start + len {
                free.remove(i);
                if at > start {
                    free.insert(i, (start, at - start));
                }
                let end = at + size;
                if end < start + len {
                    let j = if at > start { i + 1 } else { i };
                    free.insert(j, (end, start + len - end));
                }
                self.used.fetch_add(size, Relaxed);
                return Some(at);
            }
        }
        None
    }

    pub(crate) fn free(&self, (at, size): (u64, u64)) {
        let mut free = self.free.lock().unwrap_or_else(|p| p.into_inner());
        let i = free.partition_point(|&(s, _)| s < at);
        free.insert(i, (at, size));
        // Merge with the neighbours.
        if i + 1 < free.len() && free[i].0 + free[i].1 == free[i + 1].0 {
            free[i].1 += free[i + 1].1;
            free.remove(i + 1);
        }
        if i > 0 && free[i - 1].0 + free[i - 1].1 == free[i].0 {
            free[i - 1].1 += free[i].1;
            free.remove(i);
        }
        self.used.fetch_sub(size, Relaxed);
    }

    fn max_available(&self, align: u64) -> u64 {
        let align = align.max(1);
        let free = self.free.lock().unwrap_or_else(|p| p.into_inner());
        free.iter().map(|&(s, l)| (s + l).saturating_sub(s.div_ceil(align) * align)).max().unwrap_or(0)
    }
}

pub struct NmbHeap {
    pub(crate) inner: Arc<HeapInner>,
    pub(crate) device: DeviceRef,
    /// 0 automatic, 1 placement.
    pub(crate) kind: u64,
    pub(crate) options: u64,
    purgeable: AtomicU64,
}

/// `newHeapWithDescriptor:`: type (0 automatic, 1 placement), size, resource options. Sparse heaps are not made.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_heap_new(device: *mut NmbDevice, kind: u64, size: u64, options: u64) -> *mut NmbHeap {
    guard(std::ptr::null_mut(), || {
        let d = get(device)?;
        if kind > 1 {
            return Err(format!("heap type {kind}: only automatic and placement heaps are made"));
        }
        let block = d.device.take_block(Options(options).class(), size.max(1)).map_err(e("heap"))?;
        let size = block.len;
        let inner = Arc::new(HeapInner { block, size, free: Mutex::new(vec![(0, size)]), used: AtomicU64::new(0) });
        Ok(boxed(NmbHeap { inner, device: d as *const NmbDevice, kind, options, purgeable: AtomicU64::new(2) }))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_heap_release(heap: *mut NmbHeap) {
    release(heap)
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_heap_size(heap: *const NmbHeap) -> u64 {
    guard(0, || Ok(get(heap)?.inner.size))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_heap_used_size(heap: *const NmbHeap) -> u64 {
    guard(0, || Ok(get(heap)?.inner.used.load(Relaxed)))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_heap_allocated_size(heap: *const NmbHeap) -> u64 {
    guard(0, || Ok(get(heap)?.inner.size))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_heap_max_available_size(heap: *const NmbHeap, alignment: u64) -> u64 {
    guard(0, || Ok(get(heap)?.inner.max_available(alignment)))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_heap_set_purgeable_state(heap: *const NmbHeap, state: u64) -> u64 {
    guard(0, || Ok(purgeable(&get(heap)?.purgeable, state)))
}

/// A buffer in the heap: an automatic heap places it, a placement heap takes `offset` (`u64::MAX`: automatic placement).
#[unsafe(no_mangle)]
pub extern "C" fn nmb_heap_new_buffer(heap: *const NmbHeap, length: u64, options: u64, offset: u64) -> *mut NmbBuffer {
    guard(std::ptr::null_mut(), || {
        let h = get(heap)?;
        let d = device_of(h.device);
        let length = length.max(1);
        let (at, placed) = if offset == u64::MAX {
            let sa = super::device::heap_buffer(d, length, options);
            let at = h.inner.place(sa.size, sa.align).ok_or("the heap has no room")?;
            (at, Some((at, sa.size)))
        } else {
            (offset, None)
        };
        match d.device.new_heap_buffer(h.inner.range(), at, length) {
            Ok(buffer) => Ok(boxed(NmbBuffer::new(buffer, h.device, Hold::Heap(h.inner.clone(), placed), options))),
            Err(why) => {
                if let Some(range) = placed {
                    h.inner.free(range);
                }
                Err(format!("heap buffer: {why}"))
            }
        }
    })
}

/// `heapBufferSizeAndAlignWithLength:options:`.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_heap_buffer_size_and_align(device: *const NmbDevice, length: u64, options: u64, out: *mut u64) -> bool {
    guard(false, || {
        let sa = super::device::heap_buffer(get(device)?, length, options);
        let out = super::words(out as *const u64, 2, "answer")?.as_ptr() as *mut u64;
        // SAFETY: the header's two words.
        unsafe { (*out, *out.add(1)) = (sa.size, sa.align) };
        Ok(true)
    })
}
