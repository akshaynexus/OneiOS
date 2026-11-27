// SPDX-License-Identifier: MIT
//! virtio-gpu 3D on amdgpu: the DRM native context of the amdgpu protocol (virglrenderer's src/drm/amdgpu), served on
//! an amdgpu render node by raw ioctls: the Galaxy A56's Xclipse 540 (RDNA3, Linux amdgpu, /dev/dri/renderD128). No
//! libdrm and no C renderer: this is a Rust rewrite of
//!   - virglrenderer's src/drm/amdgpu/amdgpu_renderer.c (external/virglrenderer, Droid-VM's 7e17c8e: upstream's
//!     file; Copyright 2024 Advanced Micro Devices, Inc., MIT: "Permission is hereby granted, free of charge, to any
//!     person obtaining a copy of this software ... The above copyright notice and this permission notice shall be
//!     included in all copies or substantial portions of the Software." THE SOFTWARE IS PROVIDED "AS IS", WITHOUT
//!     WARRANTY OF ANY KIND), and its drm_context.c / drm_fence.c (Copyright 2022 Google LLC, MIT);
//!   - the Galaxy A56 fork's copy of it (Musicalista/OneiOS-A56 c83b9ca, runtime/xnuhost/amdgpu/src/
//!     amdgpu_renderer.c: upstream's with its responses in xnuhost's pool) and its self-test (src/gpu3d_xclipse.c,
//!     SPDX MIT);
//!   - libdrm_amdgpu's calls the C makes (libdrm 2.4.133, MIT: amdgpu_device.c, amdgpu_bo.c, amdgpu_cs.c,
//!     amdgpu_vamgr.c), as ioctls;
//!
//! serving what Mesa cecc0e9's guest sends (src/amd/common/virtio, src/virtio/vdrm; the wire in uapi.rs). Where this
//! differs from upstream, for the contract the four parts of the A56 port share (docs/a56-gpu.md, K1-K5):
//!   - K2, guest-allocated BOs: the host allocates no BO memory. GEM_NEW only records what the guest asked for (its
//!     blob_id, size, heap, flags); the RESOURCE_CREATE_BLOB that follows (HOST3D_GUEST, the same blob_id and size)
//!     brings the guest's pages, imported as udmabufs (gpu3d.rs: one or more "chunks") with PRIME_FD_TO_HANDLE. A
//!     BO is then several GEM objects of the kernel: BO_VA_OP maps each chunk at its place in the BO's range, a
//!     CS_SUBMIT's BO list names each chunk, a user fence's BO is the chunk that holds its offset. The Xclipse shares
//!     the phone's RAM, so a VRAM-preferred BO served as imported (GTT) memory is the same memory.
//!   - The capset says has_vm_always_valid = 0 (upstream 1): an imported BO cannot be a per-VM always-valid one, and
//!     with 1 Mesa makes its BOs AMDGPU_GEM_CREATE_VM_ALWAYS_VALID and leaves them out of the BO lists, so the kernel
//!     would not make them resident for a submission.
//!   - BO_QUERY_INFO answers from the GEM_NEW the guest sent and the metadata it set (upstream asks the kernel, which
//!     knows only its import: GTT, a chunk's size).
//!   - The shmem's heap figures are written once, when the shmem is made, not after every BO (upstream re-queries
//!     them, about five ioctls per BO): the host's heaps do not hold the guest's BOs.
//!   - The response shmem is carved from the drm-host pool (the fork does this too), as drm2kgsl carves its msm shmem:
//!     the guest maps pool base + offset (MAP_INFO_POOL); the pool's first 2 MiB stay unused, as drm2kgsl leaves them.
//!   - One syncobj per ring of a context, kept (upstream makes and destroys one per CS_SUBMIT), exported as a sync_file
//!     only when the guest fences the ring; one waiter thread for every ring (fences.rs; upstream one per ring).
//!   - A fence with no GPU work since the ring's previous fence retires after that one (upstream: at once, even with
//!     earlier fences of the ring still running).
//!   - No in-fence (upstream takes virgl's): the device has none to give.
//!
//! Every context opens the render node again (its own DRM file and GPU VM, as upstream: one guest process's work is
//! kept apart from another's, and the kernel's implicit sync covers what they share).
mod addon;
mod fences;
pub mod selftest;
pub mod uapi;

pub use addon::{bundle_targets, gl_in_addon, kernel_knows_gl, plugin_in_addon, Plugin, GL_PLUGIN, PLUGIN};

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use xh_core::bytes::le;

use crate::gpu3d::{self, Chunk, Entry, Errno, Gpu3d, LogFn, Retire};
use uapi::*;

/// The render node used unless the session's gpu-node= names another (the Galaxy A56's, [F] the fork's
/// docs/a56-guide.md).
pub const DEFAULT_NODE: &str = "/dev/dri/renderD128";
/// The DRM driver names taken: "amdgpu" ([F] the A56 fork's docs/a56-guide.md: "amdgpu (v3.49.0) over
/// /dev/dri/renderD128"); "sgpu", Samsung's own name for its Xclipse driver in its kernel sources, is a guess.
pub const DRIVERS: [&str; 2] = ["amdgpu", "sgpu"];
/// The pool's first bytes that are never handed out (drm2kgsl's base guard: the same guest-visible layout).
const POOL_GUARD: u64 = 2 << 20;
/// The most a QUERY_INFO may return (upstream's MAX_SIZE).
const QUERY_MAX: u32 = 1 << 20;
/// GEM_NEWs not yet followed by their blob, per context (a guest that never makes the blobs is refused past this).
const PENDING_MAX: usize = 1 << 16;

// ---- the system calls ---------------------------------------------------------------------------------------------
/// What the renderer asks of the system: the render node, its ioctls, and guest pages as dma-bufs. [`System`] on the
/// phone; the tests' stand-in for amdgpu (tests/standin.rs).
pub trait Dev: Send + Sync {
    /// Opens a node read-write, close-on-exec.
    fn open(&self, path: &str) -> Result<OwnedFd, Errno>;
    /// An ioctl.
    ///
    /// # Safety
    /// `arg` must be what `request` expects of it.
    unsafe fn ioctl(&self, fd: RawFd, request: u32, arg: *mut libc::c_void) -> Result<(), Errno>;
    /// The guest's pages of a blob as dma-bufs (gpu3d::udmabufs).
    fn chunks(&self, ram_fd: RawFd, ram_phys: u64, ram_size: u64, entries: &[Entry]) -> Result<Vec<Chunk>, Errno> {
        gpu3d::udmabufs(ram_fd, ram_phys, ram_size, entries)
    }
}

/// The system's calls.
#[derive(Debug, Default)]
pub struct System;

impl Dev for System {
    fn open(&self, path: &str) -> Result<OwnedFd, Errno> {
        let c = CString::new(path).map_err(|_| Errno(libc::EINVAL))?;
        // SAFETY: a NUL-terminated path; no O_CREAT, so no mode.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(Errno::last());
        }
        // SAFETY: a new descriptor of ours.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    unsafe fn ioctl(&self, fd: RawFd, request: u32, arg: *mut libc::c_void) -> Result<(), Errno> {
        // drmIoctl's loop: the call again on EINTR and EAGAIN (a signal, or the GPU busy at that moment).
        loop {
            // SAFETY: the caller's contract.
            match unsafe { xh_core::ioctl::ioctl(fd, request, arg) } {
                Ok(_) => return Ok(()),
                Err(e) => {
                    let n = e.raw_os_error().unwrap_or(libc::EIO);
                    if n != libc::EINTR && n != libc::EAGAIN {
                        return Err(Errno(n));
                    }
                }
            }
        }
    }
}

/// An ioctl on a structure of ours.
fn call<T>(dev: &dyn Dev, fd: RawFd, request: u32, arg: &mut T) -> Result<(), Errno> {
    // SAFETY: each caller passes the structure its request encodes.
    unsafe { dev.ioctl(fd, request, (arg as *mut T).cast()) }
}

/// An AMDGPU_INFO query into `out` (the kernel writes at most its size).
fn info<T>(dev: &dyn Dev, fd: RawFd, query: u32, arg: [u32; 4], out: &mut T) -> Result<(), Errno> {
    let mut q =
        AmdgpuInfo { return_pointer: (out as *mut T) as u64, return_size: std::mem::size_of::<T>() as u32, query, arg };
    call(dev, fd, DRM_IOCTL_AMDGPU_INFO, &mut q)
}

/// The driver's name and version (DRM_IOCTL_VERSION).
pub fn driver(dev: &dyn Dev, fd: RawFd) -> Result<(String, [i32; 3]), Errno> {
    let mut name = [0u8; 64];
    let mut v = DrmVersion {
        version_major: 0,
        version_minor: 0,
        version_patchlevel: 0,
        name_len: name.len() - 1,
        name: name.as_mut_ptr(),
        date_len: 0,
        date: std::ptr::null_mut(),
        desc_len: 0,
        desc: std::ptr::null_mut(),
    };
    call(dev, fd, DRM_IOCTL_VERSION, &mut v)?;
    let n = v.name_len.min(name.len() - 1);
    Ok((xh_core::text::nul_text(&name[..n]), [v.version_major, v.version_minor, v.version_patchlevel]))
}

/// Whether `path` is an amdgpu render node: its driver's name, or why not.
pub fn node_check(dev: &dyn Dev, path: &str) -> Result<String, String> {
    let fd = dev.open(path).map_err(|e| format!("{path}: {e}"))?;
    let (name, v) = driver(dev, fd.as_raw_fd()).map_err(|e| format!("{path}: DRM_IOCTL_VERSION: {e}"))?;
    if !DRIVERS.contains(&name.as_str()) {
        return Err(format!("{path} is the {name} driver, not amdgpu"));
    }
    Ok(format!("{name} {}.{}.{}", v[0], v[1], v[2]))
}

fn gem_close(dev: &dyn Dev, fd: RawFd, handle: u32) {
    let mut c = DrmGemClose { handle, pad: 0 };
    let _ = call(dev, fd, DRM_IOCTL_GEM_CLOSE, &mut c);
}

fn prime_import(dev: &dyn Dev, fd: RawFd, dmabuf: RawFd) -> Result<u32, Errno> {
    let mut p = DrmPrimeHandle { handle: 0, flags: 0, fd: dmabuf };
    call(dev, fd, DRM_IOCTL_PRIME_FD_TO_HANDLE, &mut p)?;
    Ok(p.handle)
}

fn neg(r: Result<(), Errno>) -> i32 {
    r.err().map_or(0, |e| -e.0)
}

// ---- what the device is -------------------------------------------------------------------------------------------
/// The node's figures, read once at the open: the capset the guest gets, the rings, the VA range.
#[derive(Clone)]
pub struct Probe {
    pub driver: String,
    pub version: [i32; 3],
    pub dev: InfoDevice,
    /// (HW IP type, its ring count) of each IP with rings, in type order: the guest's virtio ring numbers (1 up)
    /// follow this order (amdgpu_virtio_device.c virtio_ring_mapping).
    pub rings: Vec<(u32, u32)>,
    pub timelines: u32,
    pub capset: Vec<u8>,
    pub gb_addr_config: u32,
    pub marketing: String,
}

/// libdrm's amdgpu_query_sw_info(address32_hi) after amdgpu_va_manager_init2: the high 32-bit range's top when the
/// device has a high VA range, else the low one's.
pub fn address32_hi(d: &InfoDevice) -> u32 {
    let high_max = d.high_va_max.min((d.high_va_offset & !0xffff_ffff) + 0x1_0000_0000);
    if high_max != 0 {
        (high_max.wrapping_sub(1) >> 32) as u32
    } else {
        (d.virtual_address_max.min(0x1_0000_0000).wrapping_sub(1) >> 32) as u32
    }
}

/// Reads the node's figures (as virglrenderer's amdgpu_renderer_probe and amdgpu_renderer_create read them through
/// libdrm) and builds the DRM capset of K1.
pub fn probe(dev: &dyn Dev, fd: RawFd) -> Result<Probe, String> {
    let (driver, version) = driver(dev, fd).map_err(|e| format!("DRM_IOCTL_VERSION: {e}"))?;
    let mut d = InfoDevice::default();
    info(dev, fd, AMDGPU_INFO_DEV_INFO, [0; 4], &mut d).map_err(|e| format!("AMDGPU_INFO_DEV_INFO: {e}"))?;
    let reg = |offset: u32, count: u32, instance: u32, out: &mut [u32]| -> Result<(), Errno> {
        let mut q = AmdgpuInfo {
            return_pointer: out.as_mut_ptr() as u64,
            return_size: (out.len() * 4) as u32,
            query: AMDGPU_INFO_READ_MMR_REG,
            arg: [offset, count, instance, 0],
        };
        call(dev, fd, DRM_IOCTL_AMDGPU_INFO, &mut q)
    };
    // Mesa's ac_drm_query_gpu_info (libdrm's amdgpu_query_gpu_info_init): the device info, GB_ADDR_CONFIG, and on
    // GPUs older than Vega the per-SE raster and tiling registers.
    let mut c = vec![0u8; cap::SIZE];
    let g = cap::GPU_INFO;
    use cap::info as i;
    le::put_u32(&mut c, g + i::ASIC_ID, d.device_id);
    le::put_u32(&mut c, g + i::CHIP_REV, d.chip_rev);
    le::put_u32(&mut c, g + i::CHIP_EXTERNAL_REV, d.external_rev);
    le::put_u32(&mut c, g + i::FAMILY_ID, d.family);
    le::put_u64(&mut c, g + i::MAX_ENGINE_CLK, d.max_engine_clock);
    le::put_u64(&mut c, g + i::MAX_MEMORY_CLK, d.max_memory_clock);
    le::put_u32(&mut c, g + i::GPU_COUNTER_FREQ, d.gpu_counter_freq);
    le::put_u32(&mut c, g + i::ENABLED_RB_PIPES_MASK, d.enabled_rb_pipes_mask);
    le::put_u32(&mut c, g + i::RB_PIPES, d.num_rb_pipes);
    le::put_u64(&mut c, g + i::IDS_FLAGS, d.ids_flags);
    le::put_u32(&mut c, g + i::NUM_HW_GFX_CONTEXTS, d.num_hw_gfx_contexts);
    le::put_u32(&mut c, g + i::NUM_SHADER_ENGINES, d.num_shader_engines);
    le::put_u32(&mut c, g + i::NUM_SHADER_ARRAYS_PER_ENGINE, d.num_shader_arrays_per_engine);
    le::put_u32(&mut c, g + i::VRAM_TYPE, d.vram_type);
    le::put_u32(&mut c, g + i::VRAM_BIT_WIDTH, d.vram_bit_width);
    le::put_u32(&mut c, g + i::CE_RAM_SIZE, d.ce_ram_size);
    le::put_u32(&mut c, g + i::VCE_HARVEST_CONFIG, d.vce_harvest_config);
    le::put_u32(&mut c, g + i::PCI_REV_ID, d.pci_rev);
    let fail = |what: &str, e: Errno| format!("AMDGPU_INFO_READ_MMR_REG {what}: {e}");
    if d.family < AMDGPU_FAMILY_AI {
        for se in 0..d.num_shader_engines.min(4) {
            let instance = se | 0xff << 8; // AMDGPU_INFO_MMR_SE_INDEX, SH_INDEX all
            let mut v = [0u32; 1];
            reg(0x263d, 1, instance, &mut v).map_err(|e| fail("CC_RB_BACKEND_DISABLE", e))?;
            le::put_u32(&mut c, g + i::BACKEND_DISABLE + se as usize * 4, (v[0] >> 16) & 0xff);
            reg(0xa0d4, 1, instance, &mut v).map_err(|e| fail("PA_SC_RASTER_CONFIG", e))?;
            le::put_u32(&mut c, g + i::PA_SC_RASTER_CFG + se as usize * 4, v[0]);
            if d.family >= AMDGPU_FAMILY_CI {
                reg(0xa0d5, 1, instance, &mut v).map_err(|e| fail("PA_SC_RASTER_CONFIG_1", e))?;
                le::put_u32(&mut c, g + i::PA_SC_RASTER_CFG1 + se as usize * 4, v[0]);
            }
        }
    }
    let mut gb = [0u32; 1];
    reg(0x263e, 1, 0xffff_ffff, &mut gb).map_err(|e| fail("GB_ADDR_CONFIG", e))?;
    le::put_u32(&mut c, g + i::GB_ADDR_CFG, gb[0]);
    if d.family < AMDGPU_FAMILY_AI {
        let mut tiles = [0u32; 32];
        reg(0x2644, 32, 0xffff_ffff, &mut tiles).map_err(|e| fail("GB_TILE_MODE", e))?;
        for (k, t) in tiles.iter().enumerate() {
            le::put_u32(&mut c, g + i::GB_TILE_MODE + k * 4, *t);
        }
        if d.family >= AMDGPU_FAMILY_CI {
            let mut macros = [0u32; 16];
            reg(0x2664, 16, 0xffff_ffff, &mut macros).map_err(|e| fail("GB_MACROTILE_MODE", e))?;
            for (k, t) in macros.iter().enumerate() {
                le::put_u32(&mut c, g + i::GB_MACRO_TILE_MODE + k * 4, *t);
            }
        }
        let mut v = [0u32; 1];
        reg(0x9d8, 1, 0xffff_ffff, &mut v).map_err(|e| fail("MC_ARB_RAMCFG", e))?;
        le::put_u32(&mut c, g + i::MC_ARB_RAMCFG, v[0]);
    }
    le::put_u32(&mut c, g + i::CU_ACTIVE_NUMBER, d.cu_active_number);
    le::put_u32(&mut c, g + i::CU_AO_MASK, d.cu_ao_mask);
    for (k, w) in d.cu_bitmap.iter().flatten().enumerate() {
        le::put_u32(&mut c, g + i::CU_BITMAP + k * 4, *w);
    }
    // The header and the rest of u.amdgpu.
    le::put_u32(&mut c, cap::VERSION_MAJOR, version[0] as u32);
    le::put_u32(&mut c, cap::VERSION_MINOR, version[1] as u32);
    le::put_u32(&mut c, cap::VERSION_PATCHLEVEL, version[2] as u32);
    le::put_u32(&mut c, cap::CONTEXT_TYPE, cap::CONTEXT_AMDGPU);
    le::put_u32(&mut c, cap::ADDRESS32_HI, address32_hi(&d));
    le::put_u32(&mut c, cap::FLAGS, 0); // has_vm_always_valid = 0 (K2: see the file's head)
    le::put_u64(&mut c, cap::SIZE_LOCAL, d.pte_fragment_size as u64); // libdrm's amdgpu_query_buffer_size_alignment
    le::put_u64(&mut c, cap::SIZE_REMOTE, d.gart_page_size as u64);
    // libdrm names the GPU from its amdgpu.ids table, which this host does not have: the driver's own name.
    let marketing =
        if driver == "sgpu" { "Samsung Xclipse".to_owned() } else { format!("AMD Radeon ({:04x})", d.device_id) };
    let n = marketing.len().min(cap::MARKETING_NAME_LEN - 1);
    c[cap::MARKETING_NAME..cap::MARKETING_NAME + n].copy_from_slice(&marketing.as_bytes()[..n]);
    // The rings (amdgpu_renderer_create: HW_IP_INFO of each IP type, instance 0).
    let mut rings = Vec::new();
    for ip in 0..AMDGPU_HW_IP_NUM {
        let mut hw = InfoHwIp::default();
        if info(dev, fd, AMDGPU_INFO_HW_IP_INFO, [ip, 0, 0, 0], &mut hw).is_ok() && hw.available_rings != 0 {
            rings.push((ip, hw.available_rings.count_ones()));
        }
    }
    let timelines = rings.iter().map(|r| r.1).sum();
    if timelines == 0 {
        return Err("no IP has rings (AMDGPU_INFO_HW_IP_INFO)".into());
    }
    Ok(Probe { driver, version, dev: d, rings, timelines, capset: c, gb_addr_config: gb[0], marketing })
}

/// The heaps the guest is told of in its shmem (virglrenderer's update_heap_info_in_shmem through libdrm's
/// amdgpu_query_heap_info): visible VRAM, VRAM, GTT, each {size, usage, max_allocation}.
fn heaps(dev: &dyn Dev, fd: RawFd) -> [[u64; 3]; 3] {
    let mut vg = InfoVramGtt::default();
    let _ = info(dev, fd, AMDGPU_INFO_VRAM_GTT, [0; 4], &mut vg);
    let usage = |q: u32| {
        let mut v = 0u64;
        let _ = info(dev, fd, q, [0; 4], &mut v);
        v
    };
    let max = vg.vram_cpu_accessible_size;
    [
        [vg.vram_cpu_accessible_size, usage(AMDGPU_INFO_VIS_VRAM_USAGE), max],
        [vg.vram_size, usage(AMDGPU_INFO_VRAM_USAGE), max],
        [vg.gtt_size, usage(AMDGPU_INFO_GTT_USAGE), max],
    ]
}

// ---- the renderer's tables ----------------------------------------------------------------------------------------
/// What a GEM_NEW asked for (amdgpu_ccmd_gem_new_req.r).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GemNew {
    pub alloc_size: u64,
    pub phys_alignment: u64,
    pub heap: u32,
    pub flags: u64,
}

/// One chunk of a BO in a context: its GEM handle, where it is in the BO, its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub handle: u32,
    pub offset: u64,
    pub size: u64,
}

/// A BO as one context has it.
struct Obj {
    pieces: Vec<Piece>,
    size: u64,
}

#[derive(Clone, Default)]
struct Metadata {
    flags: u64,
    tiling: u64,
    data: Vec<u8>,
}

enum Kind {
    /// A context's response shmem in the pool: its offset there and its size.
    Shmem { off: u64, size: u64 },
    /// Guest pages, as udmabufs.
    Pages(Vec<Chunk>),
}

/// A resource, every context's (virglrenderer's virgl_resource).
struct Res {
    kind: Kind,
    size: u64,
    gem: Option<GemNew>,
    metadata: Option<Metadata>,
}

/// A ring of a context: its syncobj (0: none yet) and whether a CS put a fence in it since the last export.
#[derive(Default, Clone, Copy)]
struct Ring {
    syncobj: u32,
    fresh: bool,
}

struct Context {
    fd: OwnedFd,
    name: String,
    /// the response shmem: its resource, offset in the pool and size
    shmem: Option<(u32, u64, u64)>,
    objects: HashMap<u32, Obj>,
    pending: HashMap<u32, GemNew>,
    amd: HashSet<u32>,
    rings: Vec<Ring>,
}

/// First-fit extents of the pool, page-granular.
struct Pool {
    free: Vec<(u64, u64)>,
}

impl Pool {
    fn new(size: u64) -> Pool {
        Pool { free: if size > POOL_GUARD { vec![(POOL_GUARD, size - POOL_GUARD)] } else { Vec::new() } }
    }

    fn alloc(&mut self, size: u64) -> Option<u64> {
        let size = size.checked_add(0xfff)? & !0xfff;
        let i = self.free.iter().position(|&(_, s)| s >= size)?;
        let (off, s) = self.free[i];
        if s == size {
            self.free.remove(i);
        } else {
            self.free[i] = (off + size, s - size);
        }
        Some(off)
    }

    fn release(&mut self, off: u64, size: u64) {
        let size = (size + 0xfff) & !0xfff;
        let i = self.free.partition_point(|&(o, _)| o < off);
        self.free.insert(i, (off, size));
        if i + 1 < self.free.len() && self.free[i].0 + self.free[i].1 == self.free[i + 1].0 {
            self.free[i].1 += self.free[i + 1].1;
            self.free.remove(i + 1);
        }
        if i > 0 && self.free[i - 1].0 + self.free[i - 1].1 == self.free[i].0 {
            self.free[i - 1].1 += self.free[i].1;
            self.free.remove(i);
        }
    }
}

#[derive(Default)]
struct Totals {
    cs_n: u64,
    cs_sum: u64,
    cs_max: u64,
    blob_n: u64,
    blob_sum: u64,
    blob_max: u64,
    chunks: u64,
}

struct State {
    ctx: HashMap<u32, Context>,
    res: HashMap<u32, Res>,
    pool: Pool,
    t: Totals,
    since: u64,
    retired_said: u64,
    context_lines: u32,
}

/// The renderer.
pub struct Amdgpu {
    dev: Arc<dyn Dev>,
    node: String,
    probe: Probe,
    ram_fd: RawFd,
    ram_phys: u64,
    ram_size: u64,
    pool_host: usize,
    pool_size: u64,
    log: LogFn,
    errors: AtomicU32,
    st: Mutex<State>,
    fences: fences::Waiter,
    submits: AtomicU64,
}

fn now_us() -> u64 {
    xh_core::time::monotonic_ns() / 1000
}

/// An error of the guest's command or of the kernel: one line, the first 64, then every 1000th.
macro_rules! err {
    ($s:expr, $($arg:tt)*) => {{
        let k = $s.errors.fetch_add(1, Ordering::Relaxed);
        if k < 64 || k % 1000 == 0 {
            ($s.log)(0, &format!($($arg)*));
        }
    }};
}

/// A response being built (drm_context_rsp: a shadow buffer, copied to the shmem when the command is done).
struct Rsp(Vec<u8>);

impl Rsp {
    fn set_ret(&mut self, r: i32) {
        le::put_u32(&mut self.0, RSP_RET, r as u32);
    }
}

impl Amdgpu {
    /// The renderer over the guest's RAM memfd `ram_fd` (guest physical `ram_phys` at its offset 0, `ram_size`
    /// bytes), its pool at guest physical `pool_pa` (`pool_size` bytes, mapped at `pool_host`), on the render node
    /// `node`. Err(why) when the node is not amdgpu or cannot be probed.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        dev: Arc<dyn Dev>,
        node: &str,
        ram_fd: RawFd,
        ram_phys: u64,
        ram_size: u64,
        pool_host: *mut u8,
        pool_pa: u64,
        pool_size: u64,
        log: LogFn,
    ) -> Result<Amdgpu, String> {
        if pool_pa < ram_phys || pool_size > ram_size || pool_pa - ram_phys > ram_size - pool_size {
            return Err("the pool must lie in the RAM".into());
        }
        if pool_size <= POOL_GUARD || pool_host.is_null() {
            return Err("the pool must be mapped and larger than 2 MiB".into());
        }
        let fd = dev.open(node).map_err(|e| format!("{node}: {e}"))?;
        let probe = probe(&*dev, fd.as_raw_fd())?;
        if !DRIVERS.contains(&probe.driver.as_str()) {
            return Err(format!("{node} is the {} driver, not amdgpu", probe.driver));
        }
        let fences = fences::Waiter::new()?;
        let (chunk_bytes, chunk_runs) = gpu3d::udmabuf_limits();
        let d = &probe.dev;
        let rings: Vec<String> = probe.rings.iter().map(|(ip, n)| format!("{}x{n}", ip_name(*ip))).collect();
        log(
            1,
            &format!(
                "gpu: amdgpu on {node}: {} {}.{}.{}, device 0x{:04x} rev 0x{:02x} family {}, GPU addresses 0x{:x}-0x{:x} \
                 (high 0x{:x}-0x{:x}), GB_ADDR_CONFIG 0x{:08x}, rings {} ({} virtio rings); pool {} MiB; guest pages \
                 in udmabufs of up to {} MiB and {} runs",
                probe.driver,
                probe.version[0],
                probe.version[1],
                probe.version[2],
                d.device_id,
                d.pci_rev,
                d.family,
                d.virtual_address_offset,
                d.virtual_address_max,
                d.high_va_offset,
                d.high_va_max,
                probe.gb_addr_config,
                rings.join(" "),
                probe.timelines,
                pool_size >> 20,
                chunk_bytes >> 20,
                chunk_runs
            ),
        );
        Ok(Amdgpu {
            dev,
            node: node.to_owned(),
            probe,
            ram_fd,
            ram_phys,
            ram_size,
            pool_host: pool_host as usize,
            pool_size,
            log,
            errors: AtomicU32::new(0),
            st: Mutex::new(State {
                ctx: HashMap::new(),
                res: HashMap::new(),
                pool: Pool::new(pool_size),
                t: Totals::default(),
                since: 0,
                retired_said: 0,
                context_lines: 0,
            }),
            fences,
            submits: AtomicU64::new(0),
        })
    }

    pub fn probe(&self) -> &Probe {
        &self.probe
    }

    /// Fences not yet retired (the tests).
    pub fn fences_pending(&self) -> usize {
        self.fences.pending()
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- the pool, which the guest maps --------------------------------------------------------------------------
    fn pool_ptr(&self, off: u64, len: usize) -> Option<*mut u8> {
        (off <= self.pool_size && len as u64 <= self.pool_size - off)
            .then(|| (self.pool_host + off as usize) as *mut u8)
    }

    fn pool_write(&self, off: u64, bytes: &[u8]) {
        if let Some(p) = self.pool_ptr(off, bytes.len()) {
            // SAFETY: inside the pool's mapping (checked above), which lives as long as the renderer.
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len()) }
        }
    }

    fn pool_read32(&self, off: u64) -> u32 {
        match self.pool_ptr(off, 4) {
            // SAFETY: inside the pool's mapping; the guest may change it at any time, so one volatile read.
            Some(p) => unsafe { std::ptr::read_volatile(p as *const u32) },
            None => 0,
        }
    }

    fn pool_word(&self, off: u64) -> Option<&AtomicU32> {
        let p = self.pool_ptr(off, 4).filter(|p| (*p as usize) & 3 == 0)?;
        // SAFETY: an aligned word inside the pool's mapping, shared with the guest: only ever accessed atomically here.
        Some(unsafe { &*(p as *const AtomicU32) })
    }

    fn async_error(&self, c: &Context) {
        if let Some(w) = c.shmem.and_then(|(_, off, _)| self.pool_word(off + SHMEM_ASYNC_ERROR as u64)) {
            w.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// drm_context_rsp: a response of `len` bytes at the request's rsp_off, if that fits the context's shmem.
    fn rsp(&self, c: &Context, req: &[u8], len: usize) -> Result<Rsp, Errno> {
        let Some((_, _, size)) = c.shmem else { return Err(Errno(libc::ENOMEM)) };
        let room = size - SHMEM_SIZE as u64;
        let off = le::u32(req, 12) as u64;
        if off > room || len as u64 > room - off {
            err!(self, "gpu: amdgpu: a response at {off} of {len} bytes is outside the shmem ({room} bytes)");
            return Err(Errno(libc::ENOMEM));
        }
        let mut b = vec![0u8; len];
        le::put_u32(&mut b, 0, len as u32);
        Ok(Rsp(b))
    }

    /// The response into the guest's shmem: at most the length the guest put there (read once), then the seqno.
    fn respond(&self, c: &Context, req: &[u8], rsp: Option<Rsp>) {
        let Some((_, base, _)) = c.shmem else { return };
        if let Some(mut r) = rsp {
            let at = base + SHMEM_SIZE as u64 + le::u32(req, 12) as u64;
            let theirs = self.pool_read32(at) as usize;
            let n = r.0.len().min(theirs);
            if n >= 4 {
                le::put_u32(&mut r.0, 0, n as u32);
                self.pool_write(at, &r.0[..n]);
            }
        }
        if let Some(w) = self.pool_word(base + SHMEM_SEQNO as u64) {
            w.swap(le::u32(req, 8), Ordering::SeqCst);
        }
    }

    // ---- the commands ------------------------------------------------------------------------------------------
    fn dispatch(&self, st: &mut State, id: u32, cmd: u32, req: &[u8], len: usize) -> Result<Option<Rsp>, Errno> {
        let State { ctx, res, t, .. } = st;
        let c = ctx.get_mut(&id).ok_or(Errno(libc::EINVAL))?;
        match cmd {
            ccmd::QUERY_INFO => self.query_info(c, req),
            ccmd::GEM_NEW => self.gem_new(c, req),
            ccmd::BO_VA_OP => self.bo_va_op(c, req),
            ccmd::CS_SUBMIT => {
                let start = now_us();
                let r = self.cs_submit(c, req, len);
                let took = now_us() - start;
                t.cs_n += 1;
                t.cs_sum += took;
                t.cs_max = t.cs_max.max(took);
                r
            }
            ccmd::SET_METADATA => self.set_metadata(c, res, req, len),
            ccmd::BO_QUERY_INFO => self.bo_query_info(c, res, req),
            ccmd::CREATE_CTX => self.create_ctx(c, req),
            ccmd::RESERVE_VMID => self.reserve_vmid(c, req),
            ccmd::SET_PSTATE => self.set_pstate(c, req),
            ccmd::CS_QUERY_FENCE_STATUS => self.query_fence(c, req),
            _ => Err(Errno(libc::EINVAL)),
        }
    }

    fn fd(c: &Context) -> RawFd {
        c.fd.as_raw_fd()
    }

    fn query_info(&self, c: &Context, req: &[u8]) -> Result<Option<Rsp>, Errno> {
        let i = QUERY_INFO_INFO;
        let size = le::u32(req, i + 8);
        if size > QUERY_MAX {
            err!(self, "gpu: amdgpu: context {}: QUERY_INFO of {size} bytes refused", c.name);
            return Err(Errno(libc::EINVAL));
        }
        let mut rsp = self.rsp(c, req, RSP + size as usize)?;
        let mut value = vec![0u8; size as usize];
        let mut q = AmdgpuInfo {
            return_pointer: value.as_mut_ptr() as u64,
            return_size: size,
            query: le::u32(req, i + 12),
            arg: [le::u32(req, i + 16), le::u32(req, i + 20), le::u32(req, i + 24), le::u32(req, i + 28)],
        };
        let r = neg(call(&*self.dev, Self::fd(c), DRM_IOCTL_AMDGPU_INFO, &mut q));
        if r < 0 && q.query != AMDGPU_INFO_HW_IP_INFO {
            (self.log)(2, &format!("gpu: amdgpu: context {}: AMDGPU_INFO 0x{:x}: {}", c.name, q.query, Errno(-r)));
        }
        rsp.set_ret(r);
        rsp.0[RSP..].copy_from_slice(&value);
        Ok(Some(rsp))
    }

    /// K2: what the guest asked for, kept until the blob with this blob_id brings its pages. No response.
    fn gem_new(&self, c: &mut Context, req: &[u8]) -> Result<Option<Rsp>, Errno> {
        let blob_id = le::u64(req, GEM_NEW_BLOB_ID);
        let g = GemNew {
            alloc_size: le::u64(req, GEM_NEW_ALLOC_SIZE),
            phys_alignment: le::u64(req, GEM_NEW_PHYS_ALIGNMENT),
            heap: le::u32(req, GEM_NEW_PREFERRED_HEAP),
            flags: le::u64(req, GEM_NEW_FLAGS),
        };
        let why = if le::u32(req, GEM_NEW_PAD) != 0 {
            Some("its padding is not zero")
        } else if blob_id == 0 || blob_id > u32::MAX as u64 || c.pending.contains_key(&(blob_id as u32)) {
            Some("its blob_id is 0, over 32 bits, or in use")
        } else if g.alloc_size == 0 {
            Some("its size is 0")
        } else if c.pending.len() >= PENDING_MAX {
            Some("too many GEM_NEWs without their blobs")
        } else {
            None
        };
        if let Some(why) = why {
            err!(self, "gpu: amdgpu: context {}: GEM_NEW blob_id {blob_id} refused: {why}", c.name);
            self.async_error(c);
            return Err(Errno(libc::EINVAL));
        }
        c.pending.insert(blob_id as u32, g);
        Ok(None)
    }

    /// One GEM_VA of each chunk the range [offset, offset + size) of the BO covers, at its place from `va`.
    fn va_pieces(o: &Obj, offset: u64, size: u64, va: u64) -> Vec<GemVa> {
        o.pieces
            .iter()
            .filter_map(|p| {
                let a = offset.max(p.offset);
                let b = (offset + size).min(p.offset + p.size);
                (a < b).then(|| GemVa {
                    handle: p.handle,
                    pad: 0,
                    operation: 0,
                    flags: 0,
                    va_address: va + (a - offset),
                    offset_in_bo: a - p.offset,
                    map_size: b - a,
                })
            })
            .collect()
    }

    fn bo_va_op(&self, c: &Context, req: &[u8]) -> Result<Option<Rsp>, Errno> {
        let mut rsp = self.rsp(c, req, RSP)?;
        let (va, size, flags, flags2) =
            (le::u64(req, VA_OP_VA), le::u64(req, VA_OP_SIZE), le::u64(req, VA_OP_FLAGS), le::u64(req, VA_OP_FLAGS2));
        let (offset, res, op) = (le::u64(req, VA_OP_OFFSET), le::u32(req, VA_OP_RES_ID), le::u32(req, VA_OP_OP));
        if flags2 & !VA_OP_SPARSE_BO != 0 {
            err!(self, "gpu: amdgpu: context {}: BO_VA_OP flags2 0x{flags2:x} refused", c.name);
            rsp.set_ret(-libc::EINVAL);
            return Err(Errno(libc::EINVAL));
        }
        let one = |handle: u32, va: u64, offset: u64, size: u64, op: u32| {
            let mut v = GemVa {
                handle,
                pad: 0,
                operation: op,
                flags: flags as u32,
                va_address: va,
                offset_in_bo: offset,
                map_size: size,
            };
            call(&*self.dev, Self::fd(c), DRM_IOCTL_AMDGPU_GEM_VA, &mut v)
        };
        let r = if ![AMDGPU_VA_OP_MAP, AMDGPU_VA_OP_UNMAP, AMDGPU_VA_OP_CLEAR, AMDGPU_VA_OP_REPLACE].contains(&op) {
            Err(Errno(libc::EINVAL)) // libdrm's amdgpu_bo_va_op_raw
        } else if flags2 & VA_OP_SPARSE_BO != 0 {
            one(0, va, offset, size, op)
        } else {
            let Some(o) = c.objects.get(&res) else {
                err!(self, "gpu: amdgpu: context {}: BO_VA_OP op {op} of resource {res}: no such BO here", c.name);
                return Err(Errno(libc::EINVAL));
            };
            if op == AMDGPU_VA_OP_CLEAR || flags as u32 & AMDGPU_VM_PAGE_PRT != 0 {
                // The kernel takes no BO for these: the range alone.
                one(o.pieces[0].handle, va, offset, size, op)
            } else if offset.checked_add(size).is_none_or(|end| end > o.size) || size == 0 {
                Err(Errno(libc::EINVAL))
            } else {
                let pieces = Self::va_pieces(o, offset, size, va);
                let mut r = Ok(());
                for (k, p) in pieces.iter().enumerate() {
                    r = one(p.handle, p.va_address, p.offset_in_bo, p.map_size, op);
                    if r.is_err() {
                        if op == AMDGPU_VA_OP_MAP {
                            for q in &pieces[..k] {
                                let _ = one(q.handle, q.va_address, q.offset_in_bo, q.map_size, AMDGPU_VA_OP_UNMAP);
                            }
                        }
                        break;
                    }
                }
                r
            }
        };
        if let Err(e) = r {
            self.async_error(c);
            err!(
                self,
                "gpu: amdgpu: context {}: GEM_VA op {op} of resource {res} offset 0x{offset:x} size 0x{size:x} at \
                 0x{va:x}: {e}",
                c.name
            );
        }
        rsp.set_ret(neg(r));
        Ok(Some(rsp))
    }

    fn cs_submit(&self, c: &mut Context, req: &[u8], len: usize) -> Result<Option<Rsp>, Errno> {
        let mut rsp = self.rsp(c, req, RSP)?;
        let r = self.cs_submit_now(c, req, len);
        if let Err(e) = r {
            self.async_error(c);
            rsp.set_ret(-e.0);
            return Err(e);
        }
        Ok(Some(rsp))
    }

    fn cs_submit_now(&self, c: &mut Context, req: &[u8], len: usize) -> Result<(), Errno> {
        let (ctx_id, n, ring_idx) = (le::u32(req, CS_CTX_ID), le::u32(req, CS_NUM_CHUNKS), le::u32(req, CS_RING_IDX));
        let bad = |why: String| {
            err!(self, "gpu: amdgpu: context {}: CS_SUBMIT refused: {why}", c.name);
            Err(Errno(libc::EINVAL))
        };
        if n > CS_MAX_NUM_CHUNKS {
            return bad(format!("{n} chunks (at most {CS_MAX_NUM_CHUNKS})"));
        }
        if ring_idx == 0 || ring_idx > self.probe.timelines {
            return bad(format!("ring {ring_idx} (1-{})", self.probe.timelines));
        }
        if !c.amd.contains(&ctx_id) {
            return bad(format!("no amdgpu context {ctx_id}"));
        }
        let descs = CS_PAYLOAD + n as usize * CS_DESC;
        if descs > len {
            return bad(format!("{n} descriptors past the request's {len} bytes"));
        }
        // The kernel's chunks and the data they point at (owned here, 8-byte aligned).
        let mut datas: Vec<Vec<u64>> = Vec::with_capacity(n as usize + 1);
        let mut chunks: Vec<CsChunk> = Vec::with_capacity(n as usize + 1);
        let mut bo_list: Option<Vec<BoListEntry>> = None;
        let mut list_in = BoListIn::default();
        let mut fence = CsChunkFence::default();
        let words = |b: &[u8]| -> Vec<u64> {
            let mut w = vec![0u64; b.len().div_ceil(8)];
            // SAFETY: copies b's bytes into the words' bytes, which are at least as many.
            unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), w.as_mut_ptr().cast::<u8>(), b.len()) };
            w
        };
        for k in 0..n as usize {
            let d = CS_PAYLOAD + k * CS_DESC;
            let (id, dw, off) = (le::u16(req, d) as u32, le::u16(req, d + 2) as usize, le::u32(req, d + 4) as usize);
            let (at, size) = (descs + off, dw * 4);
            if at.checked_add(size).is_none_or(|end| end > len) {
                return bad(format!("chunk {k} at {at} of {size} bytes past the request's {len}"));
            }
            let data = &req[at..at + size];
            match id {
                AMDGPU_CHUNK_ID_BO_HANDLES => {
                    if at % 4 != 0 || bo_list.is_some() {
                        return bad("a second or misaligned BO list".into());
                    }
                    // K2: each resource's chunks, with its priority.
                    let mut list = Vec::new();
                    for e in data.chunks_exact(8) {
                        let (res, prio) = (le::u32(e, 0), le::u32(e, 4));
                        let Some(o) = c.objects.get(&res) else { return bad(format!("BO list: no resource {res}")) };
                        list.extend(o.pieces.iter().map(|p| BoListEntry { bo_handle: p.handle, bo_priority: prio }));
                    }
                    bo_list = Some(list);
                    chunks.push(CsChunk { chunk_id: id, length_dw: 0, chunk_data: 0 }); // filled below
                    continue;
                }
                AMDGPU_CHUNK_ID_FENCE => {
                    if size < 8 || at % 4 != 0 {
                        return bad("a fence chunk under 8 bytes".into());
                    }
                    let (res, offset) = (le::u32(data, 0), le::u32(data, 4) as u64);
                    if offset % 8 != 0 {
                        return bad(format!("user fence offset {offset} is not a multiple of 8"));
                    }
                    let Some(p) = c
                        .objects
                        .get(&res)
                        .and_then(|o| o.pieces.iter().find(|p| offset >= p.offset && offset + 8 <= p.offset + p.size))
                    else {
                        return bad(format!("user fence: no resource {res} or offset {offset} outside it"));
                    };
                    fence = CsChunkFence { handle: p.handle, offset: (offset - p.offset) as u32 };
                    chunks.push(CsChunk { chunk_id: id, length_dw: 2, chunk_data: 0 });
                    continue;
                }
                AMDGPU_CHUNK_ID_DEPENDENCIES => {
                    if at % 8 != 0 {
                        return bad("misaligned dependencies".into());
                    }
                    datas.push(words(data));
                }
                AMDGPU_CHUNK_ID_IB => {
                    if dw != std::mem::size_of::<CsChunkIb>() / 4 || at % 8 != 0 {
                        return bad(format!("an IB chunk of {dw} words"));
                    }
                    datas.push(words(data));
                }
                _ => return bad(format!("chunk id {id}")),
            }
            chunks.push(CsChunk {
                chunk_id: id,
                length_dw: dw as u32,
                chunk_data: datas.last().unwrap().as_ptr() as u64,
            });
        }
        if let Some(list) = &bo_list {
            list_in = BoListIn {
                operation: !0,
                list_handle: !0,
                bo_number: list.len() as u32,
                bo_info_size: 8,
                bo_info_ptr: list.as_ptr() as u64,
            };
        }
        for ch in chunks.iter_mut() {
            match ch.chunk_id {
                AMDGPU_CHUNK_ID_BO_HANDLES => {
                    ch.length_dw = (std::mem::size_of::<BoListIn>() / 4) as u32;
                    ch.chunk_data = (&list_in as *const BoListIn) as u64;
                }
                AMDGPU_CHUNK_ID_FENCE => ch.chunk_data = (&fence as *const CsChunkFence) as u64,
                _ => {}
            }
        }
        // The ring's syncobj takes the submission's fence (SYNCOBJ_OUT); submit_fence exports it.
        let fd = Self::fd(c);
        let ring = &mut c.rings[ring_idx as usize - 1];
        if ring.syncobj == 0 {
            let mut s = DrmSyncobj::default();
            call(&*self.dev, fd, DRM_IOCTL_SYNCOBJ_CREATE, &mut s)?;
            ring.syncobj = s.handle;
        }
        let out = CsChunkSem { handle: ring.syncobj };
        chunks.push(CsChunk {
            chunk_id: AMDGPU_CHUNK_ID_SYNCOBJ_OUT,
            length_dw: 1,
            chunk_data: (&out as *const CsChunkSem) as u64,
        });
        let ptrs: Vec<u64> = chunks.iter().map(|ch| ch as *const CsChunk as u64).collect();
        let mut cs =
            Cs { ctx_id, bo_list_handle: 0, num_chunks: ptrs.len() as u32, flags: 0, chunks: ptrs.as_ptr() as u64 };
        let r = call(&*self.dev, fd, DRM_IOCTL_AMDGPU_CS, &mut cs);
        match r {
            Ok(()) => {
                ring.fresh = true;
                self.submits.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(e) => {
                err!(
                    self,
                    "gpu: amdgpu: context {}: the submission failed: {e} (ring {ring_idx}, {} chunks, {} BOs)",
                    c.name,
                    ptrs.len(),
                    bo_list.as_ref().map_or(0, Vec::len)
                );
                Err(e)
            }
        }
    }

    fn set_metadata(
        &self,
        c: &mut Context,
        res: &mut HashMap<u32, Res>,
        req: &[u8],
        len: usize,
    ) -> Result<Option<Rsp>, Errno> {
        let mut rsp = self.rsp(c, req, RSP)?;
        let (id, size) = (le::u32(req, MD_RES_ID), le::u32(req, MD_SIZE) as usize);
        let Some(o) = c.objects.get(&id) else {
            err!(self, "gpu: amdgpu: context {}: SET_METADATA of resource {id}: no such BO here", c.name);
            rsp.set_ret(-libc::EINVAL);
            return Err(Errno(libc::EINVAL));
        };
        if size > 256 || MD_DATA + size > len {
            rsp.set_ret(-libc::EINVAL);
            return Err(Errno(libc::EINVAL));
        }
        let md = Metadata {
            flags: le::u64(req, MD_FLAGS),
            tiling: le::u64(req, MD_TILING),
            data: req[MD_DATA..MD_DATA + size].to_vec(),
        };
        let mut r = Ok(());
        for p in &o.pieces {
            let mut m = GemMetadata {
                handle: p.handle,
                op: AMDGPU_GEM_METADATA_OP_SET_METADATA,
                flags: md.flags,
                tiling_info: md.tiling,
                data_size_bytes: size as u32,
                ..Default::default()
            };
            for (k, w) in md.data.chunks(4).enumerate() {
                let mut b = [0u8; 4];
                b[..w.len()].copy_from_slice(w);
                m.data[k] = u32::from_le_bytes(b);
            }
            if r.is_ok() {
                r = call(&*self.dev, Self::fd(c), DRM_IOCTL_AMDGPU_GEM_METADATA, &mut m);
            }
        }
        if let Some(r) = res.get_mut(&id) {
            r.metadata = Some(md);
        }
        rsp.set_ret(neg(r));
        Ok(Some(rsp))
    }

    fn bo_query_info(&self, c: &Context, res: &HashMap<u32, Res>, req: &[u8]) -> Result<Option<Rsp>, Errno> {
        let mut rsp = self.rsp(c, req, BQ_RSP_SIZE)?;
        if le::u32(req, BQ_PAD) != 0 {
            rsp.set_ret(-libc::EINVAL);
            return Err(Errno(libc::EINVAL));
        }
        let id = le::u32(req, BQ_RES_ID);
        let (Some(_), Some(r)) = (c.objects.get(&id), res.get(&id)) else {
            err!(self, "gpu: amdgpu: context {}: BO_QUERY_INFO of resource {id}: no such BO here", c.name);
            rsp.set_ret(-libc::EINVAL);
            return Ok(Some(rsp));
        };
        // A blob with no GEM_NEW (a display surface): guest pages, the GTT of a UMA GPU.
        let g =
            r.gem.unwrap_or(GemNew { alloc_size: r.size, phys_alignment: 4096, heap: AMDGPU_GEM_DOMAIN_GTT, flags: 0 });
        let b = &mut rsp.0;
        le::put_u64(b, BQ_RSP_ALLOC_SIZE, g.alloc_size);
        le::put_u64(b, BQ_RSP_PHYS_ALIGNMENT, g.phys_alignment);
        le::put_u32(b, BQ_RSP_PREFERRED_HEAP, g.heap);
        le::put_u64(b, BQ_RSP_ALLOC_FLAGS, g.flags);
        if let Some(m) = &r.metadata {
            le::put_u64(b, BQ_RSP_MD_FLAGS, m.flags);
            le::put_u64(b, BQ_RSP_MD_TILING, m.tiling);
            le::put_u32(b, BQ_RSP_MD_SIZE, m.data.len() as u32);
            b[BQ_RSP_MD_DATA..BQ_RSP_MD_DATA + m.data.len()].copy_from_slice(&m.data);
        }
        Ok(Some(rsp))
    }

    fn create_ctx(&self, c: &mut Context, req: &[u8]) -> Result<Option<Rsp>, Errno> {
        let mut rsp = self.rsp(c, req, CTX_RSP_SIZE)?;
        let flags = le::u32(req, CTX_FLAGS);
        if flags & !CTX_DESTROY != 0 {
            rsp.set_ret(-libc::EINVAL);
            return Err(Errno(libc::EINVAL));
        }
        let fd = Self::fd(c);
        if flags & CTX_DESTROY == 0 {
            let mut a =
                Ctx { op: AMDGPU_CTX_OP_ALLOC_CTX, flags: 0, ctx_id: 0, priority: le::u32(req, CTX_PRIORITY) as i32 };
            let r = call(&*self.dev, fd, DRM_IOCTL_AMDGPU_CTX, &mut a);
            rsp.set_ret(neg(r));
            if let Err(e) = r {
                err!(
                    self,
                    "gpu: amdgpu: context {}: an amdgpu context (priority {}): {e}",
                    c.name,
                    le::u32(req, CTX_PRIORITY) as i32
                );
                return Ok(Some(rsp));
            }
            le::put_u32(&mut rsp.0, CTX_RSP_ID, a.op); // out.alloc.ctx_id is the union's first word
            c.amd.insert(a.op);
        } else {
            let id = le::u32(req, CTX_PRIORITY);
            if c.amd.remove(&id) {
                let mut a = Ctx { op: AMDGPU_CTX_OP_FREE_CTX, flags: 0, ctx_id: id, priority: 0 };
                let _ = call(&*self.dev, fd, DRM_IOCTL_AMDGPU_CTX, &mut a);
            } else {
                err!(self, "gpu: amdgpu: context {}: no amdgpu context {id} to free", c.name);
            }
        }
        Ok(Some(rsp))
    }

    fn reserve_vmid(&self, c: &Context, req: &[u8]) -> Result<Option<Rsp>, Errno> {
        let mut rsp = self.rsp(c, req, RSP)?;
        let flags = le::u64(req, VMID_FLAGS);
        if flags & !VMID_UNRESERVE != 0 {
            rsp.set_ret(-libc::EINVAL);
            return Err(Errno(libc::EINVAL));
        }
        let op = if flags & VMID_UNRESERVE != 0 { AMDGPU_VM_OP_UNRESERVE_VMID } else { AMDGPU_VM_OP_RESERVE_VMID };
        let mut v = Vm { op, flags: 0 };
        rsp.set_ret(neg(call(&*self.dev, Self::fd(c), DRM_IOCTL_AMDGPU_VM, &mut v)));
        Ok(Some(rsp))
    }

    fn set_pstate(&self, c: &Context, req: &[u8]) -> Result<Option<Rsp>, Errno> {
        let mut rsp = self.rsp(c, req, PSTATE_RSP_SIZE)?;
        let (id, op, flags) = (le::u32(req, PSTATE_CTX_ID), le::u32(req, PSTATE_OP), le::u32(req, PSTATE_FLAGS));
        if le::u32(req, PSTATE_PAD) != 0 || !c.amd.contains(&id) {
            rsp.set_ret(-libc::EINVAL);
            return Err(Errno(libc::EINVAL));
        }
        if op != AMDGPU_CTX_OP_GET_STABLE_PSTATE && op != AMDGPU_CTX_OP_SET_STABLE_PSTATE {
            rsp.set_ret(-libc::EINVAL);
            return Ok(Some(rsp));
        }
        let mut a = Ctx { op, flags, ctx_id: id, priority: 0 };
        let r = call(&*self.dev, Self::fd(c), DRM_IOCTL_AMDGPU_CTX, &mut a);
        rsp.set_ret(neg(r));
        if r.is_ok() {
            le::put_u32(&mut rsp.0, RSP, a.op); // out.pstate.flags is the union's first word
        }
        Ok(Some(rsp))
    }

    fn query_fence(&self, c: &Context, req: &[u8]) -> Result<Option<Rsp>, Errno> {
        let mut rsp = self.rsp(c, req, QF_RSP_SIZE)?;
        let id = le::u32(req, QF_CTX_ID);
        if !c.amd.contains(&id) {
            rsp.set_ret(-libc::EINVAL);
            return Err(Errno(libc::EINVAL));
        }
        let (ip, inst, ring, fence) =
            (le::u32(req, QF_IP_TYPE), le::u32(req, QF_IP_INSTANCE), le::u32(req, QF_RING), le::u64(req, QF_FENCE));
        let (timeout, flags) = (le::u64(req, QF_TIMEOUT), le::u64(req, QF_FLAGS));
        // libdrm's amdgpu_cs_query_fence_status and amdgpu_ioctl_wait_cs.
        if ip >= AMDGPU_HW_IP_NUM || ring >= AMDGPU_CS_MAX_RINGS {
            rsp.set_ret(-libc::EINVAL);
            return Ok(Some(rsp));
        }
        if fence == 0 {
            le::put_u32(&mut rsp.0, RSP, 1);
            return Ok(Some(rsp));
        }
        let deadline = if flags & AMDGPU_QUERY_FENCE_TIMEOUT_IS_ABSOLUTE != 0 || timeout == AMDGPU_TIMEOUT_INFINITE {
            timeout
        } else {
            let now = xh_core::time::monotonic_ns();
            timeout.checked_add(now).unwrap_or(AMDGPU_TIMEOUT_INFINITE)
        };
        let mut w = WaitCs { handle: fence, timeout: deadline, ip_type: ip, ip_instance: inst, ring, ctx_id: id };
        let r = call(&*self.dev, Self::fd(c), DRM_IOCTL_AMDGPU_WAIT_CS, &mut w);
        rsp.set_ret(neg(r));
        if r.is_ok() && w.handle == 0 {
            le::put_u32(&mut rsp.0, RSP, 1); // out.status 0: done
        }
        Ok(Some(rsp))
    }

    // ---- resources -------------------------------------------------------------------------------------------
    fn import(&self, fd: RawFd, chunks: &[Chunk]) -> Result<Vec<Piece>, Errno> {
        let mut pieces = Vec::with_capacity(chunks.len());
        let mut offset = 0;
        for ch in chunks {
            match prime_import(&*self.dev, fd, ch.fd.as_raw_fd()) {
                Ok(handle) => pieces.push(Piece { handle, offset, size: ch.size }),
                Err(e) => {
                    for p in &pieces {
                        gem_close(&*self.dev, fd, p.handle);
                    }
                    return Err(e);
                }
            }
            offset += ch.size;
        }
        Ok(pieces)
    }

    fn close_obj(&self, fd: RawFd, o: &Obj) {
        for p in &o.pieces {
            gem_close(&*self.dev, fd, p.handle);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn create_blob_now(
        &self,
        st: &mut State,
        id: u32,
        res: u32,
        mem: u32,
        flags: u32,
        blob_id: u64,
        size: u64,
        entries: &[Entry],
    ) -> Result<(), Errno> {
        if res == 0 || st.res.contains_key(&res) {
            return Err(Errno(libc::EINVAL));
        }
        let refuse = |why: &str| {
            err!(
                self,
                "gpu: amdgpu: blob {res} of context {id} refused: {why} (mem {mem}, blob_id {blob_id}, {size} bytes)"
            );
            Err(Errno(libc::EINVAL))
        };
        match mem {
            gpu3d::BLOB_HOST3D => {
                let Some(c) = st.ctx.get_mut(&id) else { return Err(Errno(libc::EINVAL)) };
                if blob_id != 0 {
                    return refuse("a host blob other than the shmem (K2: the host allocates no BO)");
                }
                if flags != gpu3d::BLOB_FLAG_MAPPABLE
                    || c.shmem.is_some()
                    || size < SHMEM_SIZE as u64
                    || size > u32::MAX as u64
                {
                    return refuse("a second shmem, or not MAPPABLE alone, or its size");
                }
                let Some(off) = st.pool.alloc(size) else { return Err(Errno(libc::ENOMEM)) };
                // The range was another context's: zeroed, then the header (drm_context_get_shmem_blob).
                self.pool_write(off, &vec![0u8; size as usize]);
                let mut head = vec![0u8; SHMEM_SIZE];
                le::put_u32(&mut head, SHMEM_RSP_MEM_OFFSET, SHMEM_SIZE as u32);
                let h = heaps(&*self.dev, c.fd.as_raw_fd());
                for (k, at) in [SHMEM_VIS_VRAM, SHMEM_VRAM, SHMEM_GTT].iter().enumerate() {
                    for (j, v) in h[k].iter().enumerate() {
                        le::put_u64(&mut head, at + j * 8, *v);
                    }
                }
                self.pool_write(off, &head);
                c.shmem = Some((res, off, size));
                st.res.insert(res, Res { kind: Kind::Shmem { off, size }, size, gem: None, metadata: None });
                Ok(())
            }
            gpu3d::BLOB_HOST3D_GUEST => {
                let Some(c) = st.ctx.get_mut(&id) else { return Err(Errno(libc::EINVAL)) };
                if blob_id == 0 || blob_id > u32::MAX as u64 {
                    return refuse("blob_id 0 or over 32 bits");
                }
                let Some(g) = c.pending.get(&(blob_id as u32)).copied() else {
                    return refuse("no GEM_NEW with this blob_id");
                };
                if g.alloc_size != size {
                    return refuse(&format!("its GEM_NEW asked for {} bytes", g.alloc_size));
                }
                c.pending.remove(&(blob_id as u32));
                let chunks = self.dev.chunks(self.ram_fd, self.ram_phys, self.ram_size, entries).inspect_err(|&e| {
                    err!(self, "gpu: amdgpu: udmabuf over {} segments of a {size} byte blob: {e}", entries.len());
                })?;
                let pieces = self.import(c.fd.as_raw_fd(), &chunks).inspect_err(|&e| {
                    err!(self, "gpu: amdgpu: context {}: PRIME import of blob {res}: {e}", c.name);
                })?;
                st.t.chunks += chunks.len() as u64;
                c.objects.insert(res, Obj { pieces, size: chunks.iter().map(|ch| ch.size).sum() });
                st.res.insert(res, Res { kind: Kind::Pages(chunks), size, gem: Some(g), metadata: None });
                Ok(())
            }
            gpu3d::BLOB_GUEST => {
                // A display surface (ctx 0): known, imported when a context attaches it.
                let chunks = self.dev.chunks(self.ram_fd, self.ram_phys, self.ram_size, entries)?;
                st.res.insert(res, Res { kind: Kind::Pages(chunks), size, gem: None, metadata: None });
                Ok(())
            }
            _ => Err(Errno(libc::EINVAL)),
        }
    }

    fn report(&self, st: &mut State, now: u64) {
        if st.since == 0 {
            st.since = now;
            return;
        }
        if now - st.since < 10_000_000 {
            return;
        }
        let f = self.fences.figures();
        let retired = f.retired.load(Ordering::Relaxed);
        let (sum, max) = (f.sum_us.swap(0, Ordering::Relaxed), f.max_us.swap(0, Ordering::Relaxed));
        let n = retired - st.retired_said;
        let t = std::mem::take(&mut st.t);
        let mean = |s: u64, n: u64| s.checked_div(n).unwrap_or(0);
        if t.cs_n != 0 || n != 0 || t.blob_n != 0 {
            (self.log)(
                1,
                &format!(
                    "gpu: {} s host: amdgpu fences {n}, fence->retired mean {} us max {max} us; submissions {}, mean {} us \
                     max {} us; blobs made {}, mean {} us max {} us ({} udmabufs)",
                    (now - st.since) / 1_000_000,
                    mean(sum, n),
                    t.cs_n,
                    mean(t.cs_sum, t.cs_n),
                    t.cs_max,
                    t.blob_n,
                    mean(t.blob_sum, t.blob_n),
                    t.blob_max,
                    t.chunks
                ),
            );
        }
        st.retired_said = retired;
        st.since = now;
    }
}

fn ip_name(ip: u32) -> &'static str {
    ["GFX", "COMPUTE", "DMA", "UVD", "VCE", "UVD_ENC", "VCN_DEC", "VCN_ENC", "VCN_JPEG", "VPE"]
        .get(ip as usize)
        .copied()
        .unwrap_or("?")
}

impl Gpu3d for Amdgpu {
    fn capset_count(&self) -> u32 {
        1
    }

    fn capset_info(&self, index: u32) -> Option<(u32, u32, u32)> {
        (index == 0).then_some((gpu3d::CAPSET_DRM, 0, cap::SIZE as u32))
    }

    fn capset(&self, id: u32, _version: u32, out: &mut [u8]) -> usize {
        let b = &self.probe.capset;
        if id != gpu3d::CAPSET_DRM || out.len() < b.len() {
            return 0;
        }
        out[..b.len()].copy_from_slice(b);
        b.len()
    }

    fn ctx_create(&self, id: u32, capset: u32, name: &[u8]) -> Result<(), Errno> {
        if capset != gpu3d::CAPSET_DRM {
            return Err(Errno(libc::EINVAL));
        }
        let mut st = self.st();
        if st.ctx.contains_key(&id) {
            return Err(Errno(libc::EEXIST));
        }
        // Each context its own DRM file and GPU VM (amdgpu_renderer_create: no libdrm device sharing).
        let fd = self.dev.open(&self.node).inspect_err(|&e| {
            err!(self, "gpu: amdgpu: context {id}: {}: {e}", self.node);
        })?;
        let name = String::from_utf8_lossy(name).into_owned();
        st.context_lines += 1;
        let k = st.context_lines;
        if k <= 400 || k % 100 == 0 {
            (self.log)(
                1,
                &format!(
                    "gpu: context {id} {name}: amdgpu, its own GPU VM on {}, {} rings",
                    self.node, self.probe.timelines
                ),
            );
        }
        st.ctx.insert(
            id,
            Context {
                fd,
                name,
                shmem: None,
                objects: HashMap::new(),
                pending: HashMap::new(),
                amd: HashSet::new(),
                rings: vec![Ring::default(); self.probe.timelines as usize],
            },
        );
        Ok(())
    }

    fn ctx_destroy(&self, id: u32) {
        let gone = self.st().ctx.remove(&id);
        if gone.is_some() {
            // Closing its DRM file frees its GEM handles, amdgpu contexts, syncobjs and VM in the kernel.
            self.fences.forget(id);
        }
        drop(gone);
    }

    fn submit(&self, id: u32, commands: &[u8]) -> Result<(), Errno> {
        let mut st = self.st();
        if !st.ctx.contains_key(&id) {
            return Err(Errno(libc::EINVAL));
        }
        let size = commands.len();
        let mut at = 0;
        while size - at >= HDR {
            let h = &commands[at..];
            let (cmd, len, rsp_off) = (le::u32(h, 0), le::u32(h, 4) as usize, le::u32(h, 12));
            if len > size - at || len < HDR || len % 8 != 0 || rsp_off % 8 != 0 {
                err!(
                    self,
                    "gpu: amdgpu: context {id}: a command of {len} bytes (rsp_off {rsp_off}) in {} left",
                    size - at
                );
                return Err(Errno(libc::EINVAL));
            }
            let Some(&need) = ccmd::REQ_SIZE.get(cmd as usize).filter(|&&n| n != 0) else {
                err!(self, "gpu: amdgpu: context {id}: unknown command {cmd}");
                return Err(Errno(libc::EINVAL));
            };
            // A copy (the guest may change its own while this runs), zero-extended to the request's size.
            let mut req = vec![0u8; len.max(need)];
            req[..len].copy_from_slice(&h[..len]);
            match self.dispatch(&mut st, id, cmd, &req, len) {
                Ok(rsp) => {
                    let c = st.ctx.get(&id).expect("the context");
                    self.respond(c, &req, rsp);
                }
                Err(e) => {
                    (self.log)(2, &format!("gpu: amdgpu: context {id}: {} failed: {e}", ccmd::NAMES[cmd as usize]));
                    return Err(e);
                }
            }
            at += len;
        }
        if at != size {
            err!(self, "gpu: amdgpu: context {id}: {} bytes after the last command", size - at);
            return Err(Errno(libc::EINVAL));
        }
        let now = now_us();
        self.report(&mut st, now);
        Ok(())
    }

    fn submit_fence(&self, id: u32, ring: u32, fence: u64) -> Result<(), Errno> {
        let fd = {
            let mut st = self.st();
            let c = st.ctx.get_mut(&id).ok_or(Errno(libc::EINVAL))?;
            if ring as usize > c.rings.len() {
                return Err(Errno(libc::EINVAL));
            }
            if ring == 0 {
                None // the guest's CPU timeline: done by now
            } else {
                let raw = c.fd.as_raw_fd();
                let r = &mut c.rings[ring as usize - 1];
                if std::mem::take(&mut r.fresh) {
                    let mut h = DrmSyncobjHandle {
                        handle: r.syncobj,
                        flags: DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE,
                        fd: -1,
                        pad: 0,
                    };
                    match call(&*self.dev, raw, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &mut h) {
                        // SAFETY: the kernel's new sync_file descriptor, ours.
                        Ok(()) if h.fd >= 0 => Some(unsafe { OwnedFd::from_raw_fd(h.fd) }),
                        r => {
                            err!(self, "gpu: amdgpu: context {id}: ring {ring}'s fence as a sync_file: {:?}", r.err());
                            None
                        }
                    }
                } else {
                    None
                }
            }
        };
        self.fences.add(id, ring, fence, fd);
        Ok(())
    }

    fn create_blob(
        &self,
        id: u32,
        res: u32,
        blob_mem: u32,
        flags: u32,
        blob_id: u64,
        size: u64,
        entries: &[Entry],
    ) -> Result<(), Errno> {
        let start = now_us();
        let mut st = self.st();
        let r = self.create_blob_now(&mut st, id, res, blob_mem, flags, blob_id, size, entries);
        let took = now_us() - start;
        st.t.blob_n += 1;
        st.t.blob_sum += took;
        st.t.blob_max = st.t.blob_max.max(took);
        r
    }

    fn map_blob(&self, res: u32) -> Result<(u64, u32), Errno> {
        match self.st().res.get(&res).map(|r| &r.kind) {
            Some(Kind::Shmem { off, .. }) => Ok((*off, gpu3d::MAP_CACHE_CACHED)),
            _ => Err(Errno(libc::EINVAL)),
        }
    }

    fn attach(&self, id: u32, res: u32, on: bool) -> Result<(), Errno> {
        let mut guard = self.st();
        let st = &mut *guard;
        let (Some(c), Some(r)) = (st.ctx.get_mut(&id), st.res.get(&res)) else { return Err(Errno(libc::EINVAL)) };
        let Kind::Pages(chunks) = &r.kind else { return Ok(()) }; // a shmem: nothing to import
        if !on {
            if let Some(o) = c.objects.remove(&res) {
                self.close_obj(c.fd.as_raw_fd(), &o);
            }
            return Ok(());
        }
        if c.objects.contains_key(&res) {
            return Ok(());
        }
        let pieces = self.import(c.fd.as_raw_fd(), chunks).inspect_err(|&e| {
            err!(self, "gpu: amdgpu: context {}: PRIME import of resource {res}: {e}", c.name);
        })?;
        let size = chunks.iter().map(|ch| ch.size).sum();
        c.objects.insert(res, Obj { pieces, size });
        Ok(())
    }

    fn unref(&self, res: u32) {
        let mut guard = self.st();
        let st = &mut *guard;
        let Some(r) = st.res.remove(&res) else { return };
        for c in st.ctx.values_mut() {
            if let Some(o) = c.objects.remove(&res) {
                self.close_obj(c.fd.as_raw_fd(), &o);
            }
            if c.shmem.is_some_and(|(s, _, _)| s == res) {
                c.shmem = None;
            }
        }
        if let Kind::Shmem { off, size } = r.kind {
            st.pool.release(off, size);
        }
    }

    fn set_retire(&self, retire: Retire) {
        self.fences.set_retire(retire);
    }
}

// ---- the choice (session/board.rs) --------------------------------------------------------------------------------
/// The 3D renderer gpu=auto or gpu=3d takes on this host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// KGSL's (drm2kgsl): today's path, its lines and its failure unchanged. A host with /dev/kgsl-3d0, or one with
    /// no amdgpu render node either.
    Kgsl,
    /// This renderer: the plug-in's oneios-target line.
    Amdgpu(String),
    /// 2D, and why (the session's "gpu: 2D only: WHY").
    TwoD(String),
    /// gpu=3d and no amdgpu renderer: do not start ("gpu: no 3D: WHY").
    Refuse(String),
}

/// The choice on a host without the gpu=2d setting: KGSL where it is (`kgsl`), else on an amdgpu render node (`node`:
/// its driver, or why it is none) this renderer when (a) the gpuaddon carries its Metal plug-in (`plugin`) and (b)
/// the self-test passes (`selftest`); each is asked only when the one before it holds.
pub fn choose(
    three_d: bool,
    kgsl: bool,
    node: Result<String, String>,
    plugin: impl FnOnce() -> Result<Plugin, String>,
    selftest: impl FnOnce() -> Result<(), String>,
) -> Choice {
    if kgsl || node.is_err() {
        return Choice::Kgsl;
    }
    let not = |why: String| if three_d { Choice::Refuse(why) } else { Choice::TwoD(why) };
    let p = match plugin() {
        Ok(p) => p,
        Err(why) => return not(why),
    };
    match selftest() {
        Ok(()) => Choice::Amdgpu(p.target),
        Err(why) => not(format!("the amdgpu self-test failed ({why})")),
    }
}

/// Before the memory is laid out: an amdgpu host without KGSL (`kgsl` false, `node` an amdgpu render node) under
/// gpu=auto whose gpuaddon has no plug-in (`plugin`, asked only then) is laid out as gpu=2d: no GPU pool, the kernel's
/// window and device tree of a 2D session (the Galaxy A56 as before this renderer). Some(why) then, for the line
/// "gpu: 2D only: WHY"; None everywhere else (KGSL hosts, other hosts, gpu=3d, a plug-in present: `choose` decides).
/// The image's build is not known yet here; `choose` checks the plug-in's target against it later.
pub fn early_2d(auto: bool, kgsl: bool, node: bool, plugin: impl FnOnce() -> Result<Plugin, String>) -> Option<String> {
    if !auto || kgsl || !node {
        return None;
    }
    plugin().err()
}

/// gpu3d::amdgpu_open's renderer: [`Amdgpu::open`] on the system.
#[allow(clippy::too_many_arguments)]
pub fn open(
    node: &str,
    ram_fd: RawFd,
    ram_phys: u64,
    ram_size: u64,
    pool_host: *mut u8,
    pool_pa: u64,
    pool_size: u64,
    log: LogFn,
) -> Result<Box<dyn Gpu3d>, String> {
    let g = Amdgpu::open(Arc::new(System), node, ram_fd, ram_phys, ram_size, pool_host, pool_pa, pool_size, log)?;
    Ok(Box::new(g))
}

// The tests: a stand-in amdgpu behind the Dev table (tests/standin.rs), host builds only.
#[cfg(all(test, not(target_os = "android")))]
mod tests;
