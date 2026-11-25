// SPDX-License-Identifier: MIT
//! The amdgpu renderer on a stand-in amdgpu (tests/standin.rs), driven as Mesa cecc0e9's guest drives it (the
//! selftest module's Guest encodes the requests): the capset, contexts, the shmem in the pool, GEM_NEW and chunked
//! guest blobs, VA maps across chunks, CS submissions (the BO list's expansion, the chunks), the fences' order, every
//! refusal; the self-test; the choice of renderer; and the layout of uapi.rs against the C headers.
mod standin;

use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use xh_core::bytes::le;

use super::selftest::{body, gfx_ring, request, Guest};
use super::*;
use standin::{Chunks, Stand, NODE};

const RAM_PHYS: u64 = 0x8_0000_0000;
const RAM_BYTES: u64 = 64 << 20;
const POOL_BYTES: u64 = 8 << 20;
const PAGE: u64 = 4096;
const SHMEM: u64 = 16384;

static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn log_line(level: u8, line: &str) {
    if std::env::var_os("TEST_VERBOSE").is_some() {
        println!("  {level} {line}");
    }
    LINES.lock().unwrap().push(line.to_owned());
}

fn said(part: &str) -> bool {
    LINES.lock().unwrap().iter().any(|l| l.contains(part))
}

#[derive(Default)]
struct Retired {
    got: Mutex<Vec<(u32, u32, u64)>>,
    cond: Condvar,
}

impl Retired {
    fn got(&self) -> Vec<(u32, u32, u64)> {
        self.got.lock().unwrap().clone()
    }
    /// Waits (at most 5 s) until `n` fences retired.
    fn wait(&self, n: usize) -> Vec<(u32, u32, u64)> {
        let g = self.got.lock().unwrap();
        let (g, _) = self.cond.wait_timeout_while(g, Duration::from_secs(5), |v| v.len() < n).unwrap();
        g.clone()
    }
}

struct Rig {
    stand: Arc<Stand>,
    gpu: Amdgpu,
    ram: *mut u8,
    _fd: OwnedFd,
    retired: Arc<Retired>,
}

impl Rig {
    fn new(chunks: Chunks) -> Rig {
        let stand = Stand::new("amdgpu", chunks);
        let fd = xh_core::fd::sealed_memfd(c"test-ram", 0, RAM_BYTES).unwrap();
        // SAFETY: a shared map of our memfd, kept for the test's life.
        let ram = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                RAM_BYTES as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        } as *mut u8;
        let pool = RAM_BYTES - POOL_BYTES;
        // SAFETY: inside the map.
        let host = unsafe { ram.add(pool as usize) };
        let gpu = Amdgpu::open(
            stand.clone(),
            NODE,
            fd.as_raw_fd(),
            RAM_PHYS,
            RAM_BYTES,
            host,
            RAM_PHYS + pool,
            POOL_BYTES,
            log_line,
        )
        .unwrap();
        let retired = Arc::new(Retired::default());
        let r = retired.clone();
        gpu.set_retire(Arc::new(move |c, ring, f| {
            r.got.lock().unwrap().push((c, ring, f));
            r.cond.notify_all();
        }));
        Rig { stand, gpu, ram, _fd: fd, retired }
    }

    /// A context with its shmem (resource 100 + ctx); a Guest over it.
    fn ctx(&self, ctx: u32, name: &str) -> Guest<'_> {
        self.gpu.ctx_create(ctx, gpu3d::CAPSET_DRM, name.as_bytes()).unwrap();
        self.gpu.create_blob(ctx, 100 + ctx, gpu3d::BLOB_HOST3D, gpu3d::BLOB_FLAG_MAPPABLE, 0, SHMEM, &[]).unwrap();
        let (off, info) = self.gpu.map_blob(100 + ctx).unwrap();
        assert_eq!(info, gpu3d::MAP_CACHE_CACHED);
        // SAFETY: the pool is the RAM's last POOL_BYTES.
        let p = unsafe { self.ram.add((RAM_BYTES - POOL_BYTES + off) as usize) };
        Guest::new(&self.gpu, ctx, p, SHMEM as usize)
    }

    /// GEM_NEW then its HOST3D_GUEST blob over `pages` guest pages each `seg` pages long, `gap` pages apart.
    fn bo(&self, g: &mut Guest<'_>, res: u32, blob_id: u64, segs: u64, seg: u64, at_page: u64) -> u64 {
        let size = segs * seg * PAGE;
        g.one(
            ccmd::GEM_NEW,
            &body::gem_new(blob_id, size, PAGE, AMDGPU_GEM_DOMAIN_VRAM, AMDGPU_GEM_CREATE_CPU_ACCESS_REQUIRED),
            0,
        )
        .unwrap();
        let entries: Vec<Entry> =
            (0..segs).map(|k| Entry::new(RAM_PHYS + (at_page + k * (seg + 1)) * PAGE, (seg * PAGE) as u32)).collect();
        self.gpu
            .create_blob(
                g.ctx,
                res,
                gpu3d::BLOB_HOST3D_GUEST,
                gpu3d::BLOB_FLAG_MAPPABLE | gpu3d::BLOB_FLAG_GUEST_HANDLE,
                blob_id,
                size,
                &entries,
            )
            .unwrap();
        size
    }

    fn file(&self, k: usize) -> i32 {
        self.stand.opened(k)
    }
}

fn amd_ctx(g: &mut Guest<'_>) -> u32 {
    let r = g.one(ccmd::CREATE_CTX, &body::create_ctx(0, 0), CTX_RSP_SIZE).unwrap();
    assert_eq!(le::u32(&r, RSP_RET), 0);
    le::u32(&r, CTX_RSP_ID)
}

fn ret(r: &[u8]) -> i32 {
    le::u32(r, RSP_RET) as i32
}

fn va_op(g: &mut Guest<'_>, va: u64, size: u64, offset: u64, res: u32, op: u32) -> i32 {
    let flags = (AMDGPU_VM_PAGE_READABLE | AMDGPU_VM_PAGE_EXECUTABLE) as u64;
    ret(&g.one(ccmd::BO_VA_OP, &body::va_op(va, size, flags, 0, offset, res, op), RSP).unwrap())
}

// ---- the capset, the open ----------------------------------------------------------------------------------------
#[test]
fn the_capset_is_the_devices() {
    let rig = Rig::new(Chunks::PerSegment);
    let g = &rig.gpu;
    assert_eq!(g.capset_count(), 1);
    assert_eq!(g.capset_info(0), Some((gpu3d::CAPSET_DRM, 0, 592)));
    assert_eq!(g.capset_info(1), None);
    let mut c = [0u8; 1024];
    assert_eq!(g.capset(gpu3d::CAPSET_DRM, 0, &mut c), 592);
    assert_eq!(g.capset(5, 0, &mut c), 0);
    assert_eq!(g.capset(gpu3d::CAPSET_DRM, 0, &mut [0u8; 100]), 0);
    let w = |at: usize| le::u32(&c, at);
    assert_eq!((w(cap::VERSION_MAJOR), w(cap::VERSION_MINOR), w(cap::VERSION_PATCHLEVEL)), (3, 49, 0));
    assert_eq!(w(cap::CONTEXT_TYPE), cap::CONTEXT_AMDGPU);
    assert_eq!(w(cap::FLAGS), 0, "has_vm_always_valid = 0 (K2)");
    // high VA 0xffff800000000000: the high 32-bit range's top is 0xffff8000ffffffff
    assert_eq!(w(cap::ADDRESS32_HI), 0xffff_8000);
    assert_eq!(le::u64(&c, cap::SIZE_LOCAL), 2 << 20);
    assert_eq!(le::u64(&c, cap::SIZE_REMOTE), 4096);
    let i = |at: usize| le::u32(&c, cap::GPU_INFO + at);
    let d = standin::device();
    assert_eq!(i(cap::info::ASIC_ID), d.device_id);
    assert_eq!(i(cap::info::FAMILY_ID), d.family);
    assert_eq!(i(cap::info::CHIP_EXTERNAL_REV), d.external_rev);
    assert_eq!(i(cap::info::GB_ADDR_CFG), standin::GB_ADDR_CONFIG);
    assert_eq!(i(cap::info::RB_PIPES), 4);
    assert_eq!(i(cap::info::VRAM_BIT_WIDTH), 64);
    assert_eq!(le::u64(&c, cap::GPU_INFO + cap::info::IDS_FLAGS), 1);
    let name = xh_core::text::nul_text(&c[cap::MARKETING_NAME..cap::MARKETING_NAME + 128]);
    assert_eq!(name, "AMD Radeon (15bf)");
    assert_eq!(g.probe().rings, vec![(0, 1), (1, 4), (2, 2)]);
    assert_eq!(g.probe().timelines, 7);
    assert_eq!(gfx_ring(g.probe()), Some(1));
    assert!(said("gpu: amdgpu on /stand-in/dri/renderD128: amdgpu 3.49.0, device 0x15bf"));
}

#[test]
fn address32_hi_without_a_high_range_is_the_low_ones() {
    let mut d = standin::device();
    d.high_va_offset = 0;
    d.high_va_max = 0;
    assert_eq!(address32_hi(&d), 0);
    d.virtual_address_max = 0x8000_0000;
    assert_eq!(address32_hi(&d), 0);
}

#[test]
fn only_an_amdgpu_node_is_taken() {
    let other = Stand::new("i915", Chunks::PerSegment);
    assert_eq!(node_check(&*other, NODE).unwrap_err(), format!("{NODE} is the i915 driver, not amdgpu"));
    assert!(node_check(&*other, "/dev/nothing").unwrap_err().starts_with("/dev/nothing: "));
    assert_eq!(node_check(&*Stand::new("sgpu", Chunks::PerSegment), NODE).unwrap(), "sgpu 3.49.0");
    let fd = xh_core::fd::sealed_memfd(c"test-ram", 0, RAM_BYTES).unwrap();
    let mut pool = vec![0u8; 16];
    let r = Amdgpu::open(
        other,
        NODE,
        fd.as_raw_fd(),
        RAM_PHYS,
        RAM_BYTES,
        pool.as_mut_ptr(),
        RAM_PHYS + RAM_BYTES - POOL_BYTES,
        POOL_BYTES,
        log_line,
    );
    assert!(r.err().unwrap().contains("is the i915 driver"));
}

// ---- contexts and the shmem --------------------------------------------------------------------------------------
#[test]
fn contexts_each_have_a_drm_file() {
    let rig = Rig::new(Chunks::PerSegment);
    let g = &rig.gpu;
    assert_eq!(rig.stand.live(), 0, "the probe's file is closed after the open");
    g.ctx_create(1, gpu3d::CAPSET_DRM, b"xnu:SpringBoard").unwrap();
    g.ctx_create(2, gpu3d::CAPSET_DRM, b"xnu:backboardd").unwrap();
    assert_eq!(g.ctx_create(2, gpu3d::CAPSET_DRM, b"again"), Err(Errno(libc::EEXIST)));
    assert_eq!(g.ctx_create(3, 5, b"venus"), Err(Errno(libc::EINVAL)));
    assert_eq!(rig.stand.live(), 2);
    assert!(said("gpu: context 2 xnu:backboardd: amdgpu, its own GPU VM on /stand-in/dri/renderD128, 7 rings"));
    g.ctx_destroy(1);
    g.ctx_destroy(1);
    assert_eq!(rig.stand.live(), 1);
    assert_eq!(g.submit(1, &request(ccmd::QUERY_INFO, &[0; 32])), Err(Errno(libc::EINVAL)));
}

#[test]
fn the_shmem_is_carved_from_the_pool() {
    let rig = Rig::new(Chunks::PerSegment);
    let g = &rig.gpu;
    let guest = rig.ctx(1, "xnu:a");
    let (off, _) = g.map_blob(101).unwrap();
    assert!(off >= 2 << 20 && off + SHMEM <= POOL_BYTES, "after the pool's first 2 MiB: {off:#x}");
    assert_eq!(guest.shmem_u32(SHMEM_RSP_MEM_OFFSET), SHMEM_SIZE as u32);
    let q = |at: usize| {
        // SAFETY: inside the shmem.
        unsafe { std::ptr::read_unaligned(guest.shmem.add(at) as *const u64) }
    };
    assert_eq!((q(SHMEM_VIS_VRAM), q(SHMEM_VRAM), q(SHMEM_GTT)), (512 << 20, 512 << 20, 4 << 30));
    assert_eq!(q(SHMEM_GTT + 8), 1 << 20);
    // One shmem a context, MAPPABLE alone, at least the header; no other host blob (K2).
    let host = |res, flags, blob_id, size| g.create_blob(1, res, gpu3d::BLOB_HOST3D, flags, blob_id, size, &[]);
    assert_eq!(host(102, gpu3d::BLOB_FLAG_MAPPABLE, 0, SHMEM), Err(Errno(libc::EINVAL)));
    g.ctx_create(2, gpu3d::CAPSET_DRM, b"xnu:b").unwrap();
    let host2 = |res, flags, blob_id, size| g.create_blob(2, res, gpu3d::BLOB_HOST3D, flags, blob_id, size, &[]);
    assert_eq!(host2(102, gpu3d::BLOB_FLAG_MAPPABLE, 7, 65536), Err(Errno(libc::EINVAL)));
    assert!(said(
        "gpu: amdgpu: blob 102 of context 2 refused: a host blob other than the shmem (K2: the host allocates no BO)"
    ));
    assert_eq!(host2(102, gpu3d::BLOB_FLAG_MAPPABLE | 4, 0, SHMEM), Err(Errno(libc::EINVAL)));
    assert_eq!(host2(102, gpu3d::BLOB_FLAG_MAPPABLE, 0, 64), Err(Errno(libc::EINVAL)));
    assert_eq!(
        g.create_blob(9, 102, gpu3d::BLOB_HOST3D, gpu3d::BLOB_FLAG_MAPPABLE, 0, SHMEM, &[]),
        Err(Errno(libc::EINVAL))
    );
    // A freed range is handed out again, zeroed.
    // SAFETY: inside the shmem.
    unsafe { std::ptr::write_volatile(guest.shmem.add(1000), 0x55u8) };
    g.unref(101);
    assert_eq!(g.map_blob(101), Err(Errno(libc::EINVAL)));
    host2(102, gpu3d::BLOB_FLAG_MAPPABLE, 0, SHMEM).unwrap();
    assert_eq!(g.map_blob(102).unwrap().0, off);
    // SAFETY: inside the pool.
    assert_eq!(unsafe { *rig.ram.add((RAM_BYTES - POOL_BYTES + off + 1000) as usize) }, 0);
    // The first context has no shmem now: commands with a response fail.
    assert_eq!(g.submit(1, &request(ccmd::CREATE_CTX, &body::create_ctx(0, 0))), Err(Errno(libc::ENOMEM)));
}

#[test]
fn query_info_is_answered_in_the_shmem() {
    let rig = Rig::new(Chunks::PerSegment);
    let mut guest = rig.ctx(1, "xnu:q");
    let size = std::mem::size_of::<InfoDevice>();
    let r =
        guest.one(ccmd::QUERY_INFO, &body::query_info(AMDGPU_INFO_DEV_INFO, size as u32, [0; 4]), RSP + size).unwrap();
    assert_eq!(ret(&r), 0);
    assert_eq!(le::u32(&r, 0) as usize, RSP + size, "the response's length");
    assert_eq!(le::u32(&r, RSP), standin::device().device_id);
    assert_eq!(guest.shmem_u32(SHMEM_SEQNO), guest.seqno());
    // A query the kernel refuses: its errno in ret, the seqno still advances.
    let r = guest.one(ccmd::QUERY_INFO, &body::query_info(0x7777, 8, [0; 4]), RSP + 8).unwrap();
    assert_eq!(ret(&r), -libc::EINVAL);
    assert_eq!(guest.shmem_u32(SHMEM_SEQNO), guest.seqno());
    // Several in one SUBMIT_3D; the guest's shorter room bounds what is written.
    let mut reqs = vec![
        (request(ccmd::QUERY_INFO, &body::query_info(AMDGPU_INFO_HW_IP_INFO, 32, [1, 0, 0, 0])), RSP + 32),
        (request(ccmd::QUERY_INFO, &body::query_info(AMDGPU_INFO_HW_IP_INFO, 32, [2, 0, 0, 0])), RSP + 32),
    ];
    let at = guest.send(&mut reqs).unwrap();
    let mask = |k: usize| guest.shmem_u32(at[k].unwrap() + RSP + 24);
    assert_eq!((mask(0), mask(1)), (0xf, 0x3));
    assert_eq!(guest.shmem_u32(SHMEM_SEQNO), guest.seqno());
    // Too large, a bad length, an unknown command, trailing bytes.
    assert_eq!(
        guest.one(ccmd::QUERY_INFO, &body::query_info(AMDGPU_INFO_DEV_INFO, (1 << 20) + 1, [0; 4]), 64).map(drop),
        Err(Errno(libc::EINVAL))
    );
    let mut bad = request(ccmd::QUERY_INFO, &[0; 32]);
    le::put_u32(&mut bad, 4, 44);
    assert_eq!(rig.gpu.submit(1, &bad), Err(Errno(libc::EINVAL)));
    assert_eq!(rig.gpu.submit(1, &request(42, &[0; 8])), Err(Errno(libc::EINVAL)));
    let mut tail = request(ccmd::CREATE_CTX, &body::create_ctx(0, 0));
    tail.extend_from_slice(&[0; 8]);
    assert_eq!(rig.gpu.submit(1, &tail), Err(Errno(libc::EINVAL)));
    // A response outside the shmem.
    let mut out = request(ccmd::CREATE_CTX, &body::create_ctx(0, 0));
    le::put_u32(&mut out, 12, SHMEM as u32);
    assert_eq!(rig.gpu.submit(1, &out), Err(Errno(libc::ENOMEM)));
}

// ---- GEM_NEW and the guest's blobs (K2) -------------------------------------------------------------------------
#[test]
fn a_guest_blob_is_its_gem_new_over_chunks() {
    let rig = Rig::new(Chunks::PerSegment);
    let mut guest = rig.ctx(1, "xnu:blob");
    let size = rig.bo(&mut guest, 5, 1, 3, 2, 64);
    assert_eq!(size, 6 * PAGE);
    let f = rig.file(1);
    assert_eq!(rig.stand.handles(f), 3, "one GEM handle a chunk");
    // BO_QUERY_INFO answers the guest's own GEM_NEW (the kernel knows only its imports: GTT, a chunk's size).
    let r = guest.one(ccmd::BO_QUERY_INFO, &[5, 0, 0, 0, 0, 0, 0, 0], BQ_RSP_SIZE).unwrap();
    assert_eq!(ret(&r), 0);
    assert_eq!(le::u64(&r, BQ_RSP_ALLOC_SIZE), 6 * PAGE);
    assert_eq!(le::u64(&r, BQ_RSP_PHYS_ALIGNMENT), PAGE);
    assert_eq!(le::u32(&r, BQ_RSP_PREFERRED_HEAP), AMDGPU_GEM_DOMAIN_VRAM);
    assert_eq!(le::u64(&r, BQ_RSP_ALLOC_FLAGS), AMDGPU_GEM_CREATE_CPU_ACCESS_REQUIRED);
    // SET_METADATA: on every chunk, and answered back.
    let mut md = vec![0u8; 24 + 8];
    le::put_u64(&mut md, 0, 3);
    le::put_u64(&mut md, 8, 0x1234);
    le::put_u32(&mut md, 16, 5);
    le::put_u32(&mut md, 20, 8);
    md[24..32].copy_from_slice(b"metadata");
    assert_eq!(ret(&guest.one(ccmd::SET_METADATA, &md, RSP).unwrap()), 0);
    let r = guest.one(ccmd::BO_QUERY_INFO, &[5, 0, 0, 0, 0, 0, 0, 0], BQ_RSP_SIZE).unwrap();
    assert_eq!(
        (le::u64(&r, BQ_RSP_MD_FLAGS), le::u64(&r, BQ_RSP_MD_TILING), le::u32(&r, BQ_RSP_MD_SIZE)),
        (3, 0x1234, 8)
    );
    assert_eq!(&r[BQ_RSP_MD_DATA..BQ_RSP_MD_DATA + 8], b"metadata");
    let handles: Vec<u32> = rig.stand.maps(f).iter().map(|m| m.handle).collect();
    assert!(handles.is_empty());
    for h in 1..=20 {
        if let Some(m) = rig.stand.metadata(f, h) {
            assert_eq!(m, (3, 0x1234, 8));
        }
    }
    // metadata too long, or past the request; an unknown BO
    let mut long = md.clone();
    le::put_u32(&mut long, 20, 300);
    assert_eq!(guest.one(ccmd::SET_METADATA, &long, RSP).map(drop), Err(Errno(libc::EINVAL)));
    le::put_u32(&mut long, 20, 64);
    assert_eq!(guest.one(ccmd::SET_METADATA, &long, RSP).map(drop), Err(Errno(libc::EINVAL)));
    le::put_u32(&mut md, 16, 77);
    assert_eq!(guest.one(ccmd::SET_METADATA, &md, RSP).map(drop), Err(Errno(libc::EINVAL)));
    let r = guest.one(ccmd::BO_QUERY_INFO, &[77, 0, 0, 0, 0, 0, 0, 0], BQ_RSP_SIZE).unwrap();
    assert_eq!(ret(&r), -libc::EINVAL);
    assert_eq!(
        guest.one(ccmd::BO_QUERY_INFO, &[5, 0, 0, 0, 1, 0, 0, 0], BQ_RSP_SIZE).map(drop),
        Err(Errno(libc::EINVAL))
    );
    // unref: every context's handles go
    rig.gpu.unref(5);
    assert_eq!(rig.stand.handles(f), 0);
}

#[test]
fn blobs_are_refused_without_their_gem_new() {
    let rig = Rig::new(Chunks::PerSegment);
    let mut guest = rig.ctx(1, "xnu:refuse");
    let g = &rig.gpu;
    let e = [Entry::new(RAM_PHYS + 64 * PAGE, PAGE as u32)];
    let guestblob = |res, blob_id, size| {
        g.create_blob(
            1,
            res,
            gpu3d::BLOB_HOST3D_GUEST,
            gpu3d::BLOB_FLAG_MAPPABLE | gpu3d::BLOB_FLAG_GUEST_HANDLE,
            blob_id,
            size,
            &e,
        )
    };
    assert_eq!(guestblob(5, 9, PAGE), Err(Errno(libc::EINVAL)), "no GEM_NEW");
    assert!(said("refused: no GEM_NEW with this blob_id"));
    assert_eq!(guestblob(5, 0, PAGE), Err(Errno(libc::EINVAL)));
    assert_eq!(guestblob(5, 1 << 33, PAGE), Err(Errno(libc::EINVAL)));
    guest.one(ccmd::GEM_NEW, &body::gem_new(9, 2 * PAGE, PAGE, AMDGPU_GEM_DOMAIN_GTT, 0), 0).unwrap();
    assert_eq!(guestblob(5, 9, PAGE), Err(Errno(libc::EINVAL)), "size mismatch");
    assert!(said("its GEM_NEW asked for 8192 bytes"));
    // the GEM_NEW stays until its blob comes
    assert_eq!(
        g.create_blob(
            1,
            5,
            gpu3d::BLOB_HOST3D_GUEST,
            9,
            9,
            2 * PAGE,
            &[Entry::new(RAM_PHYS + 64 * PAGE, 2 * PAGE as u32)]
        ),
        Ok(())
    );
    assert_eq!(guestblob(5, 9, PAGE), Err(Errno(libc::EINVAL)), "the resource id is taken");
    // GEM_NEW's own checks: padding, blob_id 0, a blob_id in use, size 0 (each fails the submission, counts an async
    // error)
    let mut pad = body::gem_new(10, PAGE, 0, AMDGPU_GEM_DOMAIN_GTT, 0);
    pad[28] = 1;
    assert_eq!(guest.one(ccmd::GEM_NEW, &pad, 0).map(drop), Err(Errno(libc::EINVAL)));
    assert_eq!(guest.one(ccmd::GEM_NEW, &body::gem_new(0, PAGE, 0, 2, 0), 0).map(drop), Err(Errno(libc::EINVAL)));
    guest.one(ccmd::GEM_NEW, &body::gem_new(11, PAGE, 0, 2, 0), 0).unwrap();
    assert_eq!(guest.one(ccmd::GEM_NEW, &body::gem_new(11, PAGE, 0, 2, 0), 0).map(drop), Err(Errno(libc::EINVAL)));
    assert_eq!(guest.one(ccmd::GEM_NEW, &body::gem_new(12, 0, 0, 2, 0), 0).map(drop), Err(Errno(libc::EINVAL)));
    assert_eq!(guest.shmem_u32(SHMEM_ASYNC_ERROR), 4);
    // unknown resources and contexts
    assert_eq!(g.attach(1, 999, true), Err(Errno(libc::EINVAL)));
    assert_eq!(g.attach(7, 5, true), Err(Errno(libc::EINVAL)));
    assert_eq!(g.map_blob(5), Err(Errno(libc::EINVAL)), "a guest blob is mapped by the guest, from its own pages");
    assert_eq!(g.create_blob(1, 6, 9, 0, 0, PAGE, &e), Err(Errno(libc::EINVAL)));
    assert_eq!(g.create_blob(1, 0, gpu3d::BLOB_GUEST, 8, 0, PAGE, &e), Err(Errno(libc::EINVAL)));
}

#[test]
fn a_display_surface_is_imported_where_it_is_attached() {
    let rig = Rig::new(Chunks::PerSegment);
    let g = &rig.gpu;
    let _a = rig.ctx(1, "xnu:app");
    let _b = rig.ctx(2, "xnu:backboardd");
    let e: Vec<Entry> = (0..4).map(|k| Entry::new(RAM_PHYS + (200 + 2 * k) * PAGE, PAGE as u32)).collect();
    g.create_blob(0, 7, gpu3d::BLOB_GUEST, gpu3d::BLOB_FLAG_GUEST_HANDLE, 0, 4 * PAGE, &e).unwrap();
    let (fa, fb) = (rig.file(1), rig.file(2));
    assert_eq!((rig.stand.handles(fa), rig.stand.handles(fb)), (0, 0));
    g.attach(2, 7, true).unwrap();
    g.attach(2, 7, true).unwrap();
    assert_eq!((rig.stand.handles(fa), rig.stand.handles(fb)), (0, 4));
    g.attach(1, 7, true).unwrap();
    assert_eq!(rig.stand.handles(fa), 4);
    // the shmem attaches as nothing
    g.attach(2, 101, true).unwrap();
    g.attach(2, 7, false).unwrap();
    assert_eq!(rig.stand.handles(fb), 0);
    g.unref(7);
    assert_eq!(rig.stand.handles(fa), 0);
}

// ---- VA maps across chunks ------------------------------------------------------------------------------------
#[test]
fn va_maps_cover_each_chunk_at_its_place() {
    let rig = Rig::new(Chunks::PerSegment);
    let mut guest = rig.ctx(1, "xnu:va");
    rig.bo(&mut guest, 5, 1, 3, 2, 64); // three chunks of 8 KiB: [0, 8K) [8K, 16K) [16K, 24K)
    let f = rig.file(1);
    let va = 0x10_0000_0000;
    assert_eq!(va_op(&mut guest, va, 6 * PAGE, 0, 5, AMDGPU_VA_OP_MAP), 0);
    let maps = rig.stand.maps(f);
    let got: Vec<(u64, u64, u64)> = maps.iter().map(|m| (m.va_address - va, m.offset_in_bo, m.map_size)).collect();
    assert_eq!(got, vec![(0, 0, 2 * PAGE), (2 * PAGE, 0, 2 * PAGE), (4 * PAGE, 0, 2 * PAGE)]);
    let flags = maps.iter().map(|m| m.flags).collect::<Vec<_>>();
    assert!(flags.iter().all(|&x| x == AMDGPU_VM_PAGE_READABLE | AMDGPU_VM_PAGE_EXECUTABLE));
    assert_eq!(va_op(&mut guest, va, 6 * PAGE, 0, 5, AMDGPU_VA_OP_UNMAP), 0);
    assert!(rig.stand.maps(f).is_empty());
    // A part of the BO that starts and ends inside chunks: [4K, 20K) at va2.
    let va2 = 0x20_0000_0000;
    assert_eq!(va_op(&mut guest, va2, 4 * PAGE, PAGE, 5, AMDGPU_VA_OP_MAP), 0);
    let got: Vec<(u64, u64, u64)> =
        rig.stand.maps(f).iter().map(|m| (m.va_address - va2, m.offset_in_bo, m.map_size)).collect();
    assert_eq!(got, vec![(0, PAGE, PAGE), (PAGE, 0, 2 * PAGE), (3 * PAGE, 0, PAGE)]);
    assert_eq!(va_op(&mut guest, va2, 4 * PAGE, PAGE, 5, AMDGPU_VA_OP_UNMAP), 0);
    // REPLACE as MAP; CLEAR and PRT ranges once, with no BO split; sparse (res 0, flags2) once with handle 0.
    let before = rig.stand.st().vas.len();
    assert_eq!(va_op(&mut guest, va, 6 * PAGE, 0, 5, AMDGPU_VA_OP_REPLACE), 0);
    assert_eq!(va_op(&mut guest, va, 6 * PAGE, 0, 5, AMDGPU_VA_OP_CLEAR), 0);
    let r = guest
        .one(
            ccmd::BO_VA_OP,
            &body::va_op(va, 6 * PAGE, AMDGPU_VM_PAGE_PRT as u64, VA_OP_SPARSE_BO, 0, 0, AMDGPU_VA_OP_MAP),
            RSP,
        )
        .unwrap();
    assert_eq!(ret(&r), 0);
    let vas: Vec<GemVa> = rig.stand.st().vas[before..].iter().map(|v| v.1).collect();
    assert_eq!(vas.len(), 3 + 1 + 1);
    assert_eq!((vas[3].operation, vas[3].map_size), (AMDGPU_VA_OP_CLEAR, 6 * PAGE));
    assert_eq!((vas[4].handle, vas[4].map_size), (0, 6 * PAGE));
    // Refusals: past the BO's end, size 0, an op that is none, an unknown BO, flags2 bits.
    assert_eq!(va_op(&mut guest, va, 7 * PAGE, 0, 5, AMDGPU_VA_OP_MAP), -libc::EINVAL);
    assert_eq!(va_op(&mut guest, va, 0, 0, 5, AMDGPU_VA_OP_MAP), -libc::EINVAL);
    assert_eq!(va_op(&mut guest, va, PAGE, 0, 5, 9), -libc::EINVAL);
    assert_eq!(
        guest.one(ccmd::BO_VA_OP, &body::va_op(va, PAGE, 0, 0, 0, 77, 1), RSP).map(drop),
        Err(Errno(libc::EINVAL))
    );
    assert_eq!(
        guest.one(ccmd::BO_VA_OP, &body::va_op(va, PAGE, 0, 2, 0, 5, 1), RSP).map(drop),
        Err(Errno(libc::EINVAL))
    );
    // A map the kernel refuses: its errno in ret, an async error counted.
    let errors = guest.shmem_u32(SHMEM_ASYNC_ERROR);
    rig.gpu.unref(5);
    assert!(rig.stand.maps(f).is_empty());
    rig.bo(&mut guest, 6, 2, 2, 1, 300);
    rig.stand.st().fail.insert(DRM_IOCTL_AMDGPU_GEM_VA, libc::ENOMEM);
    assert_eq!(va_op(&mut guest, va, 2 * PAGE, 0, 6, AMDGPU_VA_OP_MAP), -libc::ENOMEM);
    assert_eq!(guest.shmem_u32(SHMEM_ASYNC_ERROR), errors + 1);
    rig.stand.st().fail.clear();
}

#[test]
fn a_failed_piece_unmaps_the_ones_before_it() {
    let rig = Rig::new(Chunks::PerSegment);
    let mut guest = rig.ctx(1, "xnu:undo");
    rig.bo(&mut guest, 5, 1, 3, 1, 64);
    let f = rig.file(1);
    // The third chunk's handle goes from the kernel's table: its GEM_VA fails, the first two are unmapped again.
    let va = 0x30_0000_0000;
    assert_eq!(va_op(&mut guest, va, 3 * PAGE, 0, 5, AMDGPU_VA_OP_MAP), 0);
    assert_eq!(rig.stand.maps(f).len(), 3);
    let third = rig.stand.maps(f)[2].handle;
    assert_eq!(va_op(&mut guest, va, 3 * PAGE, 0, 5, AMDGPU_VA_OP_UNMAP), 0);
    let mut c = uapi::DrmGemClose { handle: third, pad: 0 };
    call(&*rig.stand, f, DRM_IOCTL_GEM_CLOSE, &mut c).unwrap();
    assert_eq!(va_op(&mut guest, va, 3 * PAGE, 0, 5, AMDGPU_VA_OP_MAP), -libc::ENOENT);
    assert!(rig.stand.maps(f).is_empty(), "the two mapped pieces are unmapped again");
    assert!(said("gpu: amdgpu: context xnu:undo: GEM_VA op 1 of resource 5"));
}

// ---- submissions ------------------------------------------------------------------------------------------------
fn nop_bo(rig: &Rig, g: &mut Guest<'_>, res: u32, blob_id: u64, va: u64) {
    rig.bo(g, res, blob_id, 3, 1, 64 + blob_id * 16);
    assert_eq!(va_op(g, va, 3 * PAGE, 0, res, AMDGPU_VA_OP_MAP), 0);
}

fn cs(g: &mut Guest<'_>, ctx: u32, ring: u32, chunks: &[(u32, Vec<u8>)]) -> Result<i32, Errno> {
    g.one(ccmd::CS_SUBMIT, &body::cs(ctx, ring, chunks), RSP).map(|r| ret(&r))
}

#[test]
fn a_submission_expands_the_bo_list_and_passes_its_chunks() {
    let rig = Rig::new(Chunks::PerSegment);
    let mut guest = rig.ctx(1, "xnu:cs");
    let actx = amd_ctx(&mut guest);
    let va = 0x40_0000_0000;
    nop_bo(&rig, &mut guest, 5, 1, va);
    rig.bo(&mut guest, 6, 2, 2, 1, 500); // a second BO, two chunks, not mapped
    let mut fence = vec![0u8; 8];
    le::put_u32(&mut fence, 0, 6);
    le::put_u32(&mut fence, 4, (PAGE + 16) as u32); // in the BO's second chunk
    let mut dep = vec![0u8; 24];
    le::put_u32(&mut dep, 12, actx);
    le::put_u64(&mut dep, 16, 1);
    let chunks = [
        (AMDGPU_CHUNK_ID_BO_HANDLES, body::bo_list(&[(5, 3), (6, 1)])),
        (AMDGPU_CHUNK_ID_FENCE, fence),
        (AMDGPU_CHUNK_ID_DEPENDENCIES, dep),
        (AMDGPU_CHUNK_ID_IB, body::ib(va + 2 * PAGE, 32, AMDGPU_HW_IP_GFX, 0)),
    ];
    assert_eq!(cs(&mut guest, actx, 1, &chunks), Ok(0));
    let s = rig.stand.st().submissions[0].clone();
    assert_eq!(s.ctx_id, actx);
    assert_eq!(s.bo_list.len(), 5, "3 chunks and 2 chunks");
    assert_eq!(s.bo_list.iter().map(|e| e.bo_priority).collect::<Vec<_>>(), vec![3, 3, 3, 1, 1]);
    assert_eq!(s.ibs, vec![CsChunkIb { va_start: va + 2 * PAGE, ib_bytes: 32, ..Default::default() }]);
    let fence = s.fence.unwrap();
    assert_eq!(fence.offset, 16, "the user fence's offset in its chunk");
    assert_eq!(fence.handle, s.bo_list[4].bo_handle, "the second chunk of BO 6");
    assert_eq!(s.deps, 1);
    assert_ne!(s.syncobj_out, 0, "the ring's syncobj takes its fence");
    // The ring's syncobj is made once.
    assert_eq!(
        cs(&mut guest, actx, 1, &chunks[..1].iter().cloned().chain([chunks[3].clone()]).collect::<Vec<_>>()),
        Ok(0)
    );
    let creates = rig.stand.st().calls.iter().filter(|&&c| c == DRM_IOCTL_SYNCOBJ_CREATE).count();
    assert_eq!(creates, 1);
    assert_eq!(rig.stand.st().submissions[1].syncobj_out, s.syncobj_out);
    // Without the BO in the list, the IB is not mapped for the GPU: the kernel (stand-in) refuses.
    assert_eq!(cs(&mut guest, actx, 1, &[(AMDGPU_CHUNK_ID_IB, body::ib(va, 32, 0, 0))]), Err(Errno(libc::EFAULT)));
    assert_eq!(guest.shmem_u32(SHMEM_ASYNC_ERROR), 1);
    assert!(said("gpu: amdgpu: context xnu:cs: the submission failed:"));
}

#[test]
fn submissions_are_refused_as_upstream_refuses_them() {
    let rig = Rig::new(Chunks::PerSegment);
    let mut guest = rig.ctx(1, "xnu:bad");
    let actx = amd_ctx(&mut guest);
    let va = 0x50_0000_0000;
    nop_bo(&rig, &mut guest, 5, 1, va);
    let ib = || (AMDGPU_CHUNK_ID_IB, body::ib(va, 32, 0, 0));
    let list = |res| (AMDGPU_CHUNK_ID_BO_HANDLES, body::bo_list(&[(res, 0)]));
    let einval = Err(Errno(libc::EINVAL));
    assert_eq!(cs(&mut guest, actx, 0, &[list(5), ib()]), einval, "ring 0 is the CPU's");
    assert_eq!(cs(&mut guest, actx, 8, &[list(5), ib()]), einval, "7 rings");
    assert_eq!(cs(&mut guest, actx + 50, 1, &[list(5), ib()]), einval, "no such amdgpu context");
    assert_eq!(cs(&mut guest, actx, 1, &[list(77), ib()]), einval, "no such resource");
    assert_eq!(cs(&mut guest, actx, 1, &[list(5), list(5), ib()]), einval, "two BO lists");
    assert_eq!(cs(&mut guest, actx, 1, &[list(5), (AMDGPU_CHUNK_ID_IB, vec![0; 24])]), einval, "an IB of 6 words");
    assert_eq!(
        cs(&mut guest, actx, 1, &[list(5), (0x0a, vec![0; 32])]),
        einval,
        "CP_GFX_SHADOW: not one Mesa forwards"
    );
    let mut fence = vec![0u8; 8];
    le::put_u32(&mut fence, 0, 5);
    le::put_u32(&mut fence, 4, 12);
    assert_eq!(cs(&mut guest, actx, 1, &[list(5), (AMDGPU_CHUNK_ID_FENCE, fence.clone()), ib()]), einval, "offset 12");
    le::put_u32(&mut fence, 4, (3 * PAGE) as u32);
    assert_eq!(cs(&mut guest, actx, 1, &[list(5), (AMDGPU_CHUNK_ID_FENCE, fence), ib()]), einval, "past the BO");
    // chunk counts: over 128, and descriptors past the request
    let many: Vec<(u32, Vec<u8>)> = (0..129).map(|_| (AMDGPU_CHUNK_ID_DEPENDENCIES, vec![0u8; 24])).collect();
    assert_eq!(cs(&mut guest, actx, 1, &many), einval);
    let mut b = body::cs(actx, 1, &[list(5), ib()]);
    le::put_u32(&mut b, 4, 9); // says 9 descriptors; the request holds 2 and their data
    assert_eq!(guest.one(ccmd::CS_SUBMIT, &b, RSP).map(drop), Err(Errno(libc::EINVAL)));
    let mut b = body::cs(actx, 1, &[list(5), ib()]);
    le::put_u32(&mut b, 16 + 8 + 4, 4096); // the IB's data offset past the end
    assert_eq!(guest.one(ccmd::CS_SUBMIT, &b, RSP).map(drop), Err(Errno(libc::EINVAL)));
    assert_eq!(rig.stand.st().submissions.len(), 0, "none reached the kernel");
    assert_eq!(guest.shmem_u32(SHMEM_ASYNC_ERROR), 12);
    assert_eq!(cs(&mut guest, actx, 1, &[list(5), ib()]), Ok(0));
}

#[test]
fn amdgpu_contexts_pstate_vmid_and_fence_queries() {
    let rig = Rig::new(Chunks::PerSegment);
    let mut guest = rig.ctx(1, "xnu:ctx");
    let a = amd_ctx(&mut guest);
    let b = amd_ctx(&mut guest);
    assert_ne!(a, b);
    // SET_PSTATE: the out flags; padding; an unknown context; an op that is not a pstate one
    let ps = |c: u32, op: u32, flags: u32, pad: u32| {
        let mut v = vec![0u8; 16];
        for (k, x) in [c, op, flags, pad].iter().enumerate() {
            le::put_u32(&mut v, k * 4, *x);
        }
        v
    };
    let r = guest.one(ccmd::SET_PSTATE, &ps(a, AMDGPU_CTX_OP_SET_STABLE_PSTATE, 2, 0), PSTATE_RSP_SIZE).unwrap();
    assert_eq!((ret(&r), le::u32(&r, RSP)), (0, 2));
    assert_eq!(guest.one(ccmd::SET_PSTATE, &ps(a, 6, 2, 1), 16).map(drop), Err(Errno(libc::EINVAL)));
    assert_eq!(guest.one(ccmd::SET_PSTATE, &ps(999, 6, 2, 0), 16).map(drop), Err(Errno(libc::EINVAL)));
    assert_eq!(ret(&guest.one(ccmd::SET_PSTATE, &ps(a, 1, 0, 0), 16).unwrap()), -libc::EINVAL);
    // RESERVE_VMID and its flags
    assert_eq!(ret(&guest.one(ccmd::RESERVE_VMID, &0u64.to_le_bytes(), RSP).unwrap()), 0);
    assert_eq!(ret(&guest.one(ccmd::RESERVE_VMID, &1u64.to_le_bytes(), RSP).unwrap()), 0);
    assert_eq!(guest.one(ccmd::RESERVE_VMID, &2u64.to_le_bytes(), RSP).map(drop), Err(Errno(libc::EINVAL)));
    // CS_QUERY_FENCE_STATUS: busy, then done; fence 0 is done; bad IP or ring
    let va = 0x60_0000_0000;
    nop_bo(&rig, &mut guest, 5, 1, va);
    let list = (AMDGPU_CHUNK_ID_BO_HANDLES, body::bo_list(&[(5, 0)]));
    assert_eq!(cs(&mut guest, a, 1, &[list, (AMDGPU_CHUNK_ID_IB, body::ib(va, 32, 0, 0))]), Ok(0));
    let qf = |c: u32, ip: u32, ring: u32, fence: u64| {
        let mut v = vec![0u8; 40];
        for (k, x) in [c, ip, 0, ring].iter().enumerate() {
            le::put_u32(&mut v, k * 4, *x);
        }
        le::put_u64(&mut v, 16, fence);
        v
    };
    let r = guest.one(ccmd::CS_QUERY_FENCE_STATUS, &qf(a, 0, 0, 1), QF_RSP_SIZE).unwrap();
    assert_eq!((ret(&r), le::u32(&r, RSP)), (0, 0));
    rig.stand.complete(0);
    let r = guest.one(ccmd::CS_QUERY_FENCE_STATUS, &qf(a, 0, 0, 1), QF_RSP_SIZE).unwrap();
    assert_eq!((ret(&r), le::u32(&r, RSP)), (0, 1));
    let r = guest.one(ccmd::CS_QUERY_FENCE_STATUS, &qf(a, 0, 0, 0), QF_RSP_SIZE).unwrap();
    assert_eq!(le::u32(&r, RSP), 1);
    assert_eq!(ret(&guest.one(ccmd::CS_QUERY_FENCE_STATUS, &qf(a, 10, 0, 1), 16).unwrap()), -libc::EINVAL);
    assert_eq!(ret(&guest.one(ccmd::CS_QUERY_FENCE_STATUS, &qf(a, 0, 8, 1), 16).unwrap()), -libc::EINVAL);
    assert_eq!(guest.one(ccmd::CS_QUERY_FENCE_STATUS, &qf(999, 0, 0, 1), 16).map(drop), Err(Errno(libc::EINVAL)));
    // CREATE_CTX: destroy; unknown flags; the priority the kernel refuses comes back in ret
    let r = guest.one(ccmd::CREATE_CTX, &body::create_ctx(b, CTX_DESTROY), CTX_RSP_SIZE).unwrap();
    assert_eq!(ret(&r), 0);
    assert_eq!(cs(&mut guest, b, 1, &[]), Err(Errno(libc::EINVAL)), "a freed context");
    assert_eq!(guest.one(ccmd::CREATE_CTX, &body::create_ctx(0, 2), CTX_RSP_SIZE).map(drop), Err(Errno(libc::EINVAL)));
    rig.stand.st().fail.insert(DRM_IOCTL_AMDGPU_CTX, libc::EACCES);
    let r = guest.one(ccmd::CREATE_CTX, &body::create_ctx(512, 0), CTX_RSP_SIZE).unwrap();
    assert_eq!((ret(&r), le::u32(&r, CTX_RSP_ID)), (-libc::EACCES, 0));
    rig.stand.st().fail.clear();
}

// ---- fences ---------------------------------------------------------------------------------------------------
#[test]
fn fences_retire_in_order_when_their_work_is_done() {
    let rig = Rig::new(Chunks::PerSegment);
    let mut guest = rig.ctx(1, "xnu:fences");
    let a = amd_ctx(&mut guest);
    let va = 0x70_0000_0000;
    nop_bo(&rig, &mut guest, 5, 1, va);
    let work = |g: &mut Guest<'_>, ring: u32| {
        let list = (AMDGPU_CHUNK_ID_BO_HANDLES, body::bo_list(&[(5, 0)]));
        cs(g, a, ring, &[list, (AMDGPU_CHUNK_ID_IB, body::ib(va, 32, 0, 0))]).unwrap()
    };
    let g = &rig.gpu;
    // ring 0 (the CPU's) and a ring with no work: at once
    g.submit_fence(1, 0, 1).unwrap();
    g.submit_fence(1, 3, 2).unwrap();
    assert_eq!(rig.retired.got(), vec![(1, 0, 1), (1, 3, 2)]);
    assert_eq!(g.submit_fence(1, 8, 3), Err(Errno(libc::EINVAL)));
    assert_eq!(g.submit_fence(9, 1, 3), Err(Errno(libc::EINVAL)));
    // two submissions on ring 1, each fenced; a third fence with no work after them
    assert_eq!(work(&mut guest, 1), 0);
    g.submit_fence(1, 1, 10).unwrap();
    assert_eq!(work(&mut guest, 1), 0);
    g.submit_fence(1, 1, 11).unwrap();
    g.submit_fence(1, 1, 12).unwrap();
    assert_eq!(rig.stand.st().exports, 2, "a sync_file only for a ring with new work");
    assert_eq!(g.fences_pending(), 3);
    // The later work finishes first: nothing retires before the earlier one.
    rig.stand.complete(1);
    rig.stand.complete(0);
    let got = rig.retired.wait(5);
    assert_eq!(got[2..], [(1, 1, 10), (1, 1, 11), (1, 1, 12)]);
    // Another ring meanwhile is its own timeline.
    assert_eq!(work(&mut guest, 6), 0);
    g.submit_fence(1, 6, 20).unwrap();
    assert_eq!(work(&mut guest, 1), 0);
    g.submit_fence(1, 1, 21).unwrap();
    rig.stand.complete(3);
    assert_eq!(rig.retired.wait(6)[5], (1, 1, 21));
    assert_eq!(g.fences_pending(), 1);
    // A context that goes takes its fences with it.
    g.ctx_destroy(1);
    assert_eq!(g.fences_pending(), 0);
    rig.stand.complete(2);
    assert_eq!(rig.retired.got().len(), 6);
}

// ---- the self-test ------------------------------------------------------------------------------------------------
#[test]
fn the_self_test_passes_on_a_working_gpu() {
    let stand = Stand::new("amdgpu", Chunks::PerSegment);
    stand.st().instant = true;
    selftest::run(stand.clone(), NODE, log_line).unwrap();
    for line in [
        "gpu: self-test: PASS raw: guest memory (a udmabuf of a sealed memfd) imported into /stand-in/dri/renderD128 and mapped at GPU address 0x",
        "gpu: self-test: PASS raw: an IB of 8 GFX NOPs submitted",
        "gpu: self-test: PASS raw: the GPU ran it",
        "gpu: self-test: PASS renderer: a context, its shmem at pool offset 0x200000, QUERY_INFO answered there (device 0x15bf)",
        "gpu: self-test: PASS renderer: GEM_NEW and a guest blob of 3 udmabufs, mapped as one BO at GPU address 0x",
        "gpu: self-test: PASS renderer: CS_SUBMIT of the NOP IB on ring 1 (GFX), its virtio fence retired through the retire callback",
    ] {
        assert!(said(line), "{line}");
    }
    // the BO's IB was in its last chunk, the list named its three chunks
    let s = stand.st().submissions.clone();
    assert_eq!(s.len(), 2);
    assert_eq!(s[1].bo_list.len(), 3);
    assert_eq!(stand.live(), 0, "every DRM file closed");
}

#[test]
fn the_self_test_fails_on_a_gpu_that_never_finishes() {
    let stand = Stand::new("amdgpu", Chunks::PerSegment);
    let e = selftest::run(stand.clone(), NODE, log_line).unwrap_err();
    assert_eq!(e, "gpu: self-test: FAIL at WAIT_CS: the GPU did not finish it in 1 s");
    let stand = Stand::new("amdgpu", Chunks::PerSegment);
    stand.st().fail.insert(DRM_IOCTL_PRIME_FD_TO_HANDLE, libc::EINVAL);
    assert!(selftest::run(stand, NODE, log_line)
        .unwrap_err()
        .starts_with("gpu: self-test: FAIL at PRIME_FD_TO_HANDLE"));
    assert!(selftest::run(Stand::new("amdgpu", Chunks::PerSegment), "/dev/none", log_line).is_err());
}

// ---- the plug-in and the choice ---------------------------------------------------------------------------------
/// A zlib stream of one stored block (enough for the reader).
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x01, 0x01];
    z.extend((data.len() as u16).to_le_bytes());
    z.extend((!(data.len() as u16)).to_le_bytes());
    z.extend_from_slice(data);
    z.extend(xh_core::inflate::adler32(data).to_be_bytes());
    z
}

/// An NGPUADD2 sidecar of these files (oneios-tool's `pack`).
fn addon(files: &[(&str, &[u8])]) -> Vec<u8> {
    let pad = |v: &mut Vec<u8>| v.resize(v.len().div_ceil(8) * 8, 0);
    let mut body = Vec::new();
    for (name, data) in files {
        let mut n = name.as_bytes().to_vec();
        n.push(0);
        let mut z = zlib_stored(data);
        body.extend((n.len() as u32).to_le_bytes());
        body.extend(0o644u32.to_le_bytes());
        body.extend((data.len() as u64).to_le_bytes());
        body.extend((z.len() as u64).to_le_bytes());
        pad(&mut n);
        pad(&mut z);
        body.extend(n);
        body.extend(z);
    }
    let mut b = b"NGPUADD2".to_vec();
    b.extend((files.len() as u32).to_le_bytes());
    b.extend(0u32.to_le_bytes());
    b.extend(((24 + body.len()) as u64).to_le_bytes());
    b.extend(body);
    b
}

const EXE: &str = "addons/OneiOSRadeonMetal.bundle/OneiOSRadeonMetal";
const TARGET: &str = "addons/OneiOSRadeonMetal.bundle/oneios-target";

#[test]
fn the_plugin_is_found_in_the_gpuaddon() {
    let good = addon(&[
        ("addons/OneiOSRadeonMetal.bundle/Info.plist", b"<plist/>"),
        (EXE, b"\xcf\xfa\xed\xfe"),
        (TARGET, b"iPhone10,3 20F66\n"),
        ("camera/MediaCapture/H9ISP.mediacapture", b"x"),
    ]);
    let p = plugin_in_addon(Some(&good), Some("20F66")).unwrap();
    assert_eq!(p, Plugin { target: "iPhone10,3 20F66".into(), files: 3 });
    assert_eq!(plugin_in_addon(Some(&good), None).unwrap().target, "iPhone10,3 20F66");
    assert_eq!(
        plugin_in_addon(Some(&good), Some("24A446")).unwrap_err(),
        "the OneiOSRadeonMetal plug-in is for \"iPhone10,3 20F66\", the image set is build 24A446"
    );
    assert_eq!(
        plugin_in_addon(None, Some("20F66")).unwrap_err(),
        "the kernel has no gpuaddon sidecar, so no OneiOSRadeonMetal plug-in (the A56's Metal)"
    );
    let adreno = addon(&[
        ("addons/NativeAdrenoMetal.bundle/NativeAdrenoMetal", b"x"),
        ("addons/NativeAdrenoMetal.publish", b"1\n"),
    ]);
    assert_eq!(
        plugin_in_addon(Some(&adreno), Some("20F66")).unwrap_err(),
        "the gpuaddon has no OneiOSRadeonMetal plug-in (the A56's Metal)"
    );
    let untargeted = addon(&[(EXE, b"x")]);
    assert_eq!(
        plugin_in_addon(Some(&untargeted), Some("20F66")).unwrap_err(),
        "the OneiOSRadeonMetal plug-in in the gpuaddon has no oneios-target"
    );
    assert_eq!(plugin_in_addon(Some(b"NGPUADD1"), None).unwrap_err(), "the gpuaddon is not NGPUADD2");
    assert_eq!(plugin_in_addon(Some(&good[..40]), None).unwrap_err(), "the gpuaddon is cut short");
}

#[test]
fn the_choice_of_renderer() {
    let ok_plugin = || Ok(Plugin { target: "iPhone10,3 20F66".into(), files: 3 });
    let never_plugin = || -> Result<Plugin, String> { panic!("not asked on a KGSL phone") };
    let never_test = || -> Result<(), String> { panic!("not asked") };
    let node = || Ok("amdgpu 3.49.0".to_owned());
    // An S24 Ultra (KGSL): KGSL, and nothing of the amdgpu path is asked (its lines, its tests: none).
    assert_eq!(choose(false, true, Err(String::new()), never_plugin, never_test), Choice::Kgsl);
    assert_eq!(choose(true, true, node(), never_plugin, never_test), Choice::Kgsl);
    // No KGSL and no amdgpu node either: KGSL's open says why, as before.
    assert_eq!(choose(false, false, Err("no".into()), never_plugin, never_test), Choice::Kgsl);
    // The A56 (gpu=auto): the plug-in and the self-test, in this order.
    assert_eq!(choose(false, false, node(), ok_plugin, || Ok(())), Choice::Amdgpu("iPhone10,3 20F66".into()));
    let no_plugin = || Err("the gpuaddon has no OneiOSRadeonMetal plug-in (the A56's Metal)".to_owned());
    assert_eq!(
        choose(false, false, node(), no_plugin, never_test),
        Choice::TwoD("the gpuaddon has no OneiOSRadeonMetal plug-in (the A56's Metal)".into())
    );
    assert_eq!(
        choose(false, false, node(), ok_plugin, || Err("gpu: self-test: FAIL at WAIT_CS: x".into())),
        Choice::TwoD("the amdgpu self-test failed (gpu: self-test: FAIL at WAIT_CS: x)".into())
    );
    // gpu=3d: this renderer or do not start.
    assert_eq!(choose(true, false, node(), ok_plugin, || Ok(())), Choice::Amdgpu("iPhone10,3 20F66".into()));
    assert!(matches!(choose(true, false, node(), no_plugin, never_test), Choice::Refuse(_)));
    assert!(matches!(choose(true, false, node(), ok_plugin, || Err("x".into())), Choice::Refuse(_)));
}

// ---- guest pages through the real /dev/udmabuf ------------------------------------------------------------------
#[test]
fn a_blob_past_udmabufs_run_limit_is_two_bos_chunks() {
    if std::fs::OpenOptions::new().read(true).write(true).open("/dev/udmabuf").is_err() {
        println!("amdgpu on a stand-in: the udmabuf blob: skipped (no /dev/udmabuf here)");
        return;
    }
    let rig = Rig::new(Chunks::Udmabuf);
    let mut guest = rig.ctx(1, "xnu:udmabuf");
    // 1100 separate pages: two udmabufs (1024 runs and 76), as gpu3d.rs splits them on the phone
    let size = rig.bo(&mut guest, 5, 1, 1100, 1, 1024);
    let f = rig.file(1);
    assert!(rig.stand.handles(f) >= 2, "1100 runs: more than one udmabuf");
    let va = 0x80_0000_0000;
    assert_eq!(va_op(&mut guest, va, size, 0, 5, AMDGPU_VA_OP_MAP), 0);
    // (another test may have set smaller limits meanwhile: more chunks, the same cover)
    let maps = rig.stand.maps(f);
    assert!(maps.len() >= 2 && maps.len() == rig.stand.handles(f));
    let mut next = 0;
    for m in &maps {
        assert_eq!((m.va_address - va, m.offset_in_bo), (next, 0));
        next += m.map_size;
    }
    assert_eq!(next, size);
}

// ---- the layout of uapi.rs against the C headers ------------------------------------------------------------------
#[cfg(amdgpu_layout)]
extern "C" {
    fn oneios_amdgpu_layout(out: *mut *const u64, bitfield: *mut u32) -> usize;
}

#[test]
fn the_layout_is_the_headers() {
    #[cfg(not(amdgpu_layout))]
    println!("amdgpu layout: skipped (no external/mesa-droidvm or no libdrm amdgpu.h: build.rs built no layout.c)");
    #[cfg(amdgpu_layout)]
    {
        use std::mem::{offset_of, size_of};
        let (mut p, mut bit) = (std::ptr::null(), 0u32);
        // SAFETY: the C gives its static array and length.
        let n = unsafe { oneios_amdgpu_layout(&mut p, &mut bit) };
        // SAFETY: as above; the array is static.
        let c = unsafe { std::slice::from_raw_parts(p, n) };
        assert_eq!(bit, 1, "has_vm_always_valid is bit 0 of the word after address32_hi");
        let ours: Vec<u64> = [
            // sizes
            size_of::<DrmVersion>(),
            size_of::<DrmPrimeHandle>(),
            size_of::<DrmGemClose>(),
            size_of::<DrmSyncobj>(),
            size_of::<DrmSyncobj>(),
            size_of::<DrmSyncobjHandle>(),
            size_of::<AmdgpuInfo>(),
            size_of::<Ctx>(),
            size_of::<BoListIn>(),
            size_of::<BoListIn>(),
            size_of::<BoListEntry>(),
            size_of::<CsChunk>(),
            size_of::<Cs>(),
            size_of::<CsChunkIb>(),
            size_of::<CsChunkFence>(),
            24,
            size_of::<CsChunkSem>(),
            size_of::<WaitCs>(),
            size_of::<GemMetadata>(),
            size_of::<Vm>(),
            size_of::<InfoDevice>(),
            size_of::<InfoHwIp>(),
            size_of::<InfoVramGtt>(),
            // offsets
            offset_of!(DrmVersion, name_len),
            offset_of!(DrmVersion, desc),
            offset_of!(DrmSyncobjHandle, fd),
            offset_of!(AmdgpuInfo, query),
            offset_of!(AmdgpuInfo, arg),
            offset_of!(AmdgpuInfo, arg) + 8,
            offset_of!(GemVa, operation),
            offset_of!(GemVa, va_address),
            offset_of!(GemVa, offset_in_bo),
            offset_of!(GemVa, map_size),
            offset_of!(Ctx, priority),
            0,
            0,
            0,
            offset_of!(BoListIn, bo_info_ptr),
            offset_of!(Cs, chunks),
            0,
            offset_of!(CsChunkIb, va_start),
            offset_of!(CsChunkIb, ring),
            offset_of!(WaitCs, ctx_id),
            0,
            offset_of!(GemMetadata, flags),
            offset_of!(GemMetadata, data_size_bytes),
            offset_of!(GemMetadata, data),
            offset_of!(InfoDevice, family),
            offset_of!(InfoDevice, cu_bitmap),
            offset_of!(InfoDevice, ids_flags),
            offset_of!(InfoDevice, virtual_address_offset),
            offset_of!(InfoDevice, virtual_address_max),
            offset_of!(InfoDevice, virtual_address_alignment),
            offset_of!(InfoDevice, pte_fragment_size),
            offset_of!(InfoDevice, gart_page_size),
            offset_of!(InfoDevice, vram_type),
            offset_of!(InfoDevice, high_va_offset),
            offset_of!(InfoDevice, high_va_max),
            offset_of!(InfoDevice, tcc_disabled_mask),
            offset_of!(InfoDevice, userq_ip_mask),
            offset_of!(InfoHwIp, available_rings),
        ]
        .iter()
        .map(|&v| v as u64)
        .chain([
            DRM_IOCTL_VERSION as u64,
            DRM_IOCTL_GEM_CLOSE as u64,
            DRM_IOCTL_PRIME_FD_TO_HANDLE as u64,
            DRM_IOCTL_SYNCOBJ_CREATE as u64,
            DRM_IOCTL_SYNCOBJ_DESTROY as u64,
            DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD as u64,
            DRM_IOCTL_AMDGPU_CTX as u64,
            DRM_IOCTL_AMDGPU_BO_LIST as u64,
            DRM_IOCTL_AMDGPU_CS as u64,
            DRM_IOCTL_AMDGPU_INFO as u64,
            DRM_IOCTL_AMDGPU_GEM_METADATA as u64,
            (DRM_IOCTL_AMDGPU_GEM_VA & 0xff) as u64,
            (DRM_IOCTL_AMDGPU_GEM_VA >> 30) as u64,
            DRM_IOCTL_AMDGPU_WAIT_CS as u64,
            DRM_IOCTL_AMDGPU_VM as u64,
        ])
        .chain([
            DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE as u64,
            AMDGPU_GEM_DOMAIN_GTT as u64,
            AMDGPU_GEM_DOMAIN_VRAM as u64,
            AMDGPU_GEM_CREATE_CPU_ACCESS_REQUIRED,
            AMDGPU_GEM_CREATE_VM_ALWAYS_VALID,
            AMDGPU_BO_LIST_OP_CREATE as u64,
            AMDGPU_BO_LIST_OP_DESTROY as u64,
            AMDGPU_CTX_OP_ALLOC_CTX as u64,
            AMDGPU_CTX_OP_FREE_CTX as u64,
            AMDGPU_CTX_OP_GET_STABLE_PSTATE as u64,
            AMDGPU_CTX_OP_SET_STABLE_PSTATE as u64,
            AMDGPU_VM_OP_RESERVE_VMID as u64,
            AMDGPU_VM_OP_UNRESERVE_VMID as u64,
            AMDGPU_GEM_METADATA_OP_SET_METADATA as u64,
            AMDGPU_VA_OP_MAP as u64,
            AMDGPU_VA_OP_UNMAP as u64,
            AMDGPU_VA_OP_CLEAR as u64,
            AMDGPU_VA_OP_REPLACE as u64,
            AMDGPU_VM_PAGE_READABLE as u64,
            AMDGPU_VM_PAGE_EXECUTABLE as u64,
            AMDGPU_VM_PAGE_PRT as u64,
            AMDGPU_HW_IP_GFX as u64,
            AMDGPU_HW_IP_NUM as u64,
            AMDGPU_CHUNK_ID_IB as u64,
            AMDGPU_CHUNK_ID_FENCE as u64,
            AMDGPU_CHUNK_ID_DEPENDENCIES as u64,
            AMDGPU_CHUNK_ID_SYNCOBJ_OUT as u64,
            AMDGPU_CHUNK_ID_BO_HANDLES as u64,
            AMDGPU_INFO_HW_IP_INFO as u64,
            AMDGPU_INFO_VRAM_USAGE as u64,
            AMDGPU_INFO_GTT_USAGE as u64,
            AMDGPU_INFO_VRAM_GTT as u64,
            AMDGPU_INFO_READ_MMR_REG as u64,
            AMDGPU_INFO_DEV_INFO as u64,
            AMDGPU_INFO_VIS_VRAM_USAGE as u64,
            AMDGPU_FAMILY_AI as u64,
            AMDGPU_FAMILY_CI as u64,
            AMDGPU_TIMEOUT_INFINITE,
            AMDGPU_QUERY_FENCE_TIMEOUT_IS_ABSOLUTE,
        ])
        .chain(1..=10u64)
        .chain(ccmd::REQ_SIZE[1..].iter().map(|&s| s as u64))
        .chain(
            [
                HDR,
                RSP,
                RSP_RET,
                QUERY_INFO_INFO,
                RSP,
                GEM_NEW_BLOB_ID,
                GEM_NEW_ALLOC_SIZE,
                GEM_NEW_PHYS_ALIGNMENT,
                GEM_NEW_PREFERRED_HEAP,
                GEM_NEW_PAD,
                GEM_NEW_FLAGS,
                VA_OP_VA,
                VA_OP_SIZE,
                VA_OP_FLAGS,
                VA_OP_FLAGS2,
                VA_OP_OFFSET,
                VA_OP_RES_ID,
                VA_OP_OP,
                VA_OP_SPARSE_BO as usize,
                CS_CTX_ID,
                CS_NUM_CHUNKS,
                CS_RING_IDX,
                CS_PAYLOAD,
                CS_MAX_NUM_CHUNKS as usize,
                MD_FLAGS,
                MD_TILING,
                MD_RES_ID,
                MD_SIZE,
                MD_DATA,
                BQ_RES_ID,
                BQ_PAD,
                BQ_RSP_ALLOC_SIZE,
                BQ_RSP_PHYS_ALIGNMENT,
                BQ_RSP_PREFERRED_HEAP,
                BQ_RSP_ALLOC_FLAGS,
                BQ_RSP_MD_FLAGS,
                BQ_RSP_MD_TILING,
                BQ_RSP_MD_SIZE,
                BQ_RSP_MD_DATA,
                BQ_RSP_SIZE,
                CTX_PRIORITY,
                CTX_FLAGS,
                CTX_DESTROY as usize,
                CTX_RSP_ID,
                CTX_RSP_SIZE,
                VMID_FLAGS,
                VMID_UNRESERVE as usize,
                PSTATE_CTX_ID,
                PSTATE_OP,
                PSTATE_FLAGS,
                PSTATE_PAD,
                PSTATE_RSP_SIZE,
                QF_CTX_ID,
                QF_IP_TYPE,
                QF_IP_INSTANCE,
                QF_RING,
                QF_FENCE,
                QF_TIMEOUT,
                QF_FLAGS,
                QF_RSP_SIZE,
                SHMEM_SEQNO,
                SHMEM_RSP_MEM_OFFSET,
                SHMEM_ASYNC_ERROR,
                SHMEM_GTT,
                SHMEM_VRAM,
                SHMEM_VIS_VRAM,
                SHMEM_SIZE,
                24,
                cap::VERSION_MAJOR,
                cap::VERSION_MINOR,
                cap::VERSION_PATCHLEVEL,
                cap::CONTEXT_TYPE,
                cap::ADDRESS32_HI,
                cap::SIZE_LOCAL,
                cap::SIZE_REMOTE,
                cap::GPU_INFO,
                cap::MARKETING_NAME,
                cap::MARKETING_NAME_LEN,
                cap::SIZE,
                cap::CONTEXT_AMDGPU as usize,
            ]
            .iter()
            .map(|&v| v as u64),
        )
        .chain(
            {
                use cap::info::*;
                [
                    ASIC_ID,
                    CHIP_REV,
                    CHIP_EXTERNAL_REV,
                    FAMILY_ID,
                    IDS_FLAGS,
                    MAX_ENGINE_CLK,
                    MAX_MEMORY_CLK,
                    NUM_SHADER_ENGINES,
                    NUM_SHADER_ARRAYS_PER_ENGINE,
                    NUM_HW_GFX_CONTEXTS,
                    RB_PIPES,
                    ENABLED_RB_PIPES_MASK,
                    GPU_COUNTER_FREQ,
                    BACKEND_DISABLE,
                    MC_ARB_RAMCFG,
                    GB_ADDR_CFG,
                    GB_TILE_MODE,
                    GB_MACRO_TILE_MODE,
                    PA_SC_RASTER_CFG,
                    PA_SC_RASTER_CFG1,
                    CU_ACTIVE_NUMBER,
                    CU_AO_MASK,
                    CU_BITMAP,
                    VRAM_TYPE,
                    VRAM_BIT_WIDTH,
                    CE_RAM_SIZE,
                    VCE_HARVEST_CONFIG,
                    PCI_REV_ID,
                    SIZE,
                ]
            }
            .iter()
            .map(|&v| v as u64),
        )
        .collect();
        assert_eq!(ours.len(), c.len(), "layout.c and this list hold the same entries");
        for (k, (a, b)) in ours.iter().zip(c).enumerate() {
            assert_eq!(a, b, "entry {k} of layout.c: ours {a:#x}, the header's {b:#x}");
        }
        assert_eq!(ccmd::NAMES.len(), ccmd::REQ_SIZE.len());
    }
}

/// Today's A56 (gpu=auto, no A56 plug-in in the gpuaddon) is laid out as gpu=2d: no GPU pool, the same window and device
/// tree as before this renderer. Everywhere else the layout is decided as before (the pool is kept) and `choose` decides.
#[test]
fn an_a56_without_its_plugin_is_laid_out_as_2d() {
    let ok_plugin = || Ok(Plugin { target: "iPhone10,3 20F66".into(), files: 3 });
    let never = || -> Result<Plugin, String> { panic!("not asked") };
    let why = "the gpuaddon has no OneiOSRadeonMetal plug-in (the A56's Metal)";
    assert_eq!(early_2d(true, false, true, || Err(why.to_owned())), Some(why.to_owned()));
    assert_eq!(early_2d(true, false, true, ok_plugin), None);
    // an S24 Ultra (KGSL), a host with no amdgpu node, gpu=3d (or not auto): the addon is not even read
    assert_eq!(early_2d(true, true, true, never), None);
    assert_eq!(early_2d(true, false, false, never), None);
    assert_eq!(early_2d(false, false, true, never), None);
}

#[test]
fn the_gl_bundle_is_found_in_the_gpuaddon_beside_the_metal_plugin() {
    // the A56's OpenGL ES (docs/a56-gpu.md 4a): addons/OneiOSRadeonGL.bundle/OneiOSRadeonGL and its oneios-target, beside
    // the Metal plug-in in the same sidecar (oneios-tool ios gpuaddon BUNDLE OUT --gl GLBUNDLE)
    let both = addon(&[
        ("addons/OneiOSRadeonMetal.bundle/Info.plist", b"<plist/>"),
        (EXE, b"\xcf\xfa\xed\xfe"),
        (TARGET, b"iPhone10,3 20F66\n"),
        ("addons/OneiOSRadeonMetal.publish", b"1\n"),
        ("addons/OneiOSRadeonGL.bundle/Info.plist", b"<plist/>"),
        ("addons/OneiOSRadeonGL.bundle/OneiOSRadeonGL", b"\xcf\xfa\xed\xfe"),
        ("addons/OneiOSRadeonGL.bundle/oneios-target", b"iPhone10,3 20F66\n"),
    ]);
    assert_eq!(
        gl_in_addon(Some(&both), Some("20F66")).unwrap(),
        Plugin { target: "iPhone10,3 20F66".into(), files: 3 }
    );
    // the Metal plug-in's answer is unchanged by the bundle beside it
    assert_eq!(plugin_in_addon(Some(&both), Some("20F66")).unwrap().files, 3);
    let metal_only = addon(&[(EXE, b"x"), (TARGET, b"iPhone10,3 20F66\n")]);
    assert_eq!(
        gl_in_addon(Some(&metal_only), Some("20F66")).unwrap_err(),
        "the gpuaddon has no OneiOSRadeonGL plug-in (the A56's OpenGL ES)"
    );
    assert_eq!(
        gl_in_addon(None, None).unwrap_err(),
        "the kernel has no gpuaddon sidecar, so no OneiOSRadeonGL plug-in (the A56's OpenGL ES)"
    );
    assert_eq!(
        gl_in_addon(Some(&both), Some("24A446")).unwrap_err(),
        "the OneiOSRadeonGL plug-in is for \"iPhone10,3 20F66\", the image set is build 24A446"
    );
}

/// The A56's OpenGL ES is offered only to a kernel that reads oneios-gpu-amd-gl (5966e71ce2aa on): on the pinned
/// 181d8bf2d60a SpringBoard would find no GL driver with the CoreImage fixup left out.
#[test]
fn opengl_es_needs_a_kernel_that_reads_its_switch() {
    let mut kc = vec![0u8; 4096];
    kc.extend_from_slice(b"oneios-gpu-amd\0oneios-gpu-amd-cpu\0");
    assert!(!kernel_knows_gl(&kc), "181d8bf2d60a's strings: no oneios-gpu-amd-gl");
    kc.extend_from_slice(b"oneios-gpu-amd-gl\0");
    assert!(kernel_knows_gl(&kc));
    assert!(!kernel_knows_gl(b"oneios-gpu-amd-g"));
}
