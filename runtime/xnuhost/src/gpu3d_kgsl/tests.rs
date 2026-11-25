// SPDX-License-Identifier: MIT
//! The renderer (gpu3d_kgsl.rs with the patched drm2kgsl) on the build host against a stand-in for KGSL (kgsl.rs): the
//! ports of tests/test_drm2kgsl.c (the scenarios) and tests/test_gpu_leaks.c (every fd and KGSL object back through
//! many blob cycles, failed blobs and a reset; the heap no larger after a second round of the cycles than after the
//! first, where the C asked LeakSanitizer). Each scenario runs in a process of its own (the renderer is one per
//! process): the test binary runs itself again with XH_GPU_SCENARIO naming it. Without /dev/udmabuf (root:kvm on a PC)
//! only the policy's scenarios run, on a stand-in for it; the memory scenarios need the real one.
mod kgsl;

use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Mutex;
use xh_core::bytes::le;

use kgsl::{stand, RANGE_OP_BIND, RANGE_OP_UNBIND, VA_BASE};
use policy::{
    HINT_VALID, QOS_USER_INITIATED, QOS_USER_INTERACTIVE, QOS_UTILITY, ROLE_BACKGROUND, ROLE_FOREGROUND, ROLE_NONUI,
};

use super::*;
use crate::gpu3d::{
    Entry, Gpu3d, BLOB_FLAG_GUEST_HANDLE, BLOB_FLAG_MAPPABLE, BLOB_GUEST, BLOB_HOST3D, BLOB_HOST3D_GUEST, CAPSET_DRM,
};

const SCENARIO: &str = "XH_GPU_SCENARIO";

/// Runs `name` (this test's own name) in a child process; in that child, runs `body`.
fn scenario(name: &str, body: fn()) {
    match std::env::var(SCENARIO) {
        Ok(s) if s == name => {
            // The C's child _exit()ed: a normal exit runs the C's atexit handlers while drm2kgsl's threads run.
            let ok = std::panic::catch_unwind(body).is_ok();
            use std::io::Write;
            let _ = std::io::stdout().flush();
            // SAFETY: the end of this child process.
            unsafe { libc::_exit(if ok { 0 } else { 101 }) }
        }
        Ok(_) => return, // another scenario's child
        Err(_) => {}
    }
    if !drm2kgsl_sys::AVAILABLE {
        println!("drm2kgsl on a stand-in KGSL: {name}: skipped (no drm2kgsl in this build)");
        return;
    }
    let udmabuf = std::fs::OpenOptions::new().read(true).write(true).open("/dev/udmabuf").is_ok();
    let policy = matches!(name, "classes" | "shown_surfaces");
    if !udmabuf && !policy {
        println!("drm2kgsl on a stand-in KGSL: {name}: skipped (no /dev/udmabuf here: the memory scenarios need it)");
        return;
    }
    let me = std::env::current_exe().unwrap();
    let status = std::process::Command::new(me)
        .args([format!("gpu3d_kgsl::tests::{name}").as_str(), "--exact", "--nocapture", "--test-threads=1"])
        .env(SCENARIO, name)
        .env("XH_GPU_FAKE_UDMABUF", if udmabuf { "0" } else { "1" })
        .status()
        .unwrap();
    assert!(status.success(), "drm2kgsl on a stand-in KGSL: {name}: {status}");
}

// ---- a guest context, as gpuctl drives one -----------------------------------------------------------------------------
const RAM_PHYS: u64 = 0x8_0000_0000;
const RAM_BYTES: u64 = 128 << 20;
const POOL_BYTES: u64 = 8 << 20;

static LAST_ERROR: Mutex<String> = Mutex::new(String::new());
/// the last "gpu: context" line (the policy's)
static LAST_CONTEXT: Mutex<String> = Mutex::new(String::new());

fn log_line(level: u8, line: &str) {
    if level == 0 {
        *LAST_ERROR.lock().unwrap() = line.to_owned();
    }
    if line.starts_with("gpu: context") {
        *LAST_CONTEXT.lock().unwrap() = line.to_owned();
    }
    if std::env::var_os("TEST_VERBOSE").is_some() {
        println!("  {level} {line}");
    }
}

/// A look at the stand-in under one guard (two guards in one expression would wait for each other).
fn with<T>(f: impl FnOnce(&kgsl::Stand) -> T) -> T {
    f(&stand())
}

fn last_context() -> String {
    LAST_CONTEXT.lock().unwrap().clone()
}

/// The renderer over a memfd of RAM_BYTES as the guest's RAM, the pool at its end.
struct Renderer {
    gpu: Box<dyn Gpu3d>,
    ram: *mut u8,
    _fd: OwnedFd,
    va_start: u64,
    va_size: u64,
}

fn renderer(profile: kgsl::Profile) -> Renderer {
    kgsl::install(profile, std::env::var("XH_GPU_FAKE_UDMABUF").as_deref() == Ok("1"));
    // A memfd sized and sealed against shrinking (udmabuf wants it).
    let fd = xh_core::fd::sealed_memfd(c"test-ram", 0, RAM_BYTES).unwrap();
    // SAFETY: a shared map of our memfd, kept for the process's life.
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
    let gpu = match gpu3d::kgsl_open(fd.as_raw_fd(), RAM_PHYS, RAM_BYTES, host, RAM_PHYS + pool, POOL_BYTES, log_line) {
        Ok(g) => g,
        Err(why) => panic!("the renderer: {why}"),
    };
    let mut bytes = [0u8; 4096];
    let n = gpu.capset(CAPSET_DRM, 0, &mut bytes);
    assert!(n >= 48);
    // wire_format, major, minor, patch, context_type, pad, has_cached_coherent, priorities; va start, size
    let va_start = le::u64(&bytes, 32);
    let va_size = le::u64(&bytes, 40);
    Renderer { gpu, ram, _fd: fd, va_start, va_size }
}

/// A request of the msm protocol: its bytes, little-endian, whole words.
struct Req(Vec<u8>);

impl Req {
    /// vdrm_ccmd_req: cmd, len, seqno, rsp_off (filled at the submit)
    fn new(cmd: u32) -> Req {
        let mut r = Req(Vec::new());
        r.u32(cmd).u32(0).u32(0).u32(0);
        r
    }
    fn u32(&mut self, v: u32) -> &mut Req {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u64(&mut self, v: u64) -> &mut Req {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn bytes(&mut self, b: &[u8]) -> &mut Req {
        self.0.extend_from_slice(b);
        self
    }
}

/// One guest context: its id, its shmem (seqno, rsp_mem_offset, async_error, global_faults), its slice of the VBO.
struct Context {
    id: u32,
    shmem: *mut u8,
    seqno: std::cell::Cell<u32>,
    slice: u64,
}

impl Context {
    fn shmem_u32(&self, i: usize) -> u32 {
        // SAFETY: the shmem lies in the pool, mapped for the test's life.
        unsafe { (self.shmem.add(i * 4) as *const u32).read_volatile() }
    }
    fn async_error(&self) -> u32 {
        self.shmem_u32(2)
    }
    /// The guest's reset of async_error after it read it.
    fn clear_async_error(&self) {
        // SAFETY: the shmem lies in the pool, mapped for the test's life.
        unsafe { (self.shmem.add(8) as *mut u32).write_volatile(0) }
    }
    fn rsp(&self) -> *mut u8 {
        // SAFETY: rsp_mem_offset lies inside the shmem.
        unsafe { self.shmem.add(self.shmem_u32(1) as usize) }
    }
    fn rsp_read<const N: usize>(&self, at: usize) -> [u8; N] {
        let mut b = [0u8; N];
        // SAFETY: inside the response area.
        unsafe { std::ptr::copy_nonoverlapping(self.rsp().add(at), b.as_mut_ptr(), N) };
        b
    }
    fn rsp_want(&self, len: u32) {
        // SAFETY: inside the response area.
        unsafe { (self.rsp() as *mut u32).write_unaligned(len) };
    }
}

impl Renderer {
    fn ccmd(&self, c: &Context, mut req: Req) -> Result<(), i32> {
        c.seqno.set(c.seqno.get() + 1);
        let len = req.0.len() as u32;
        req.0[4..8].copy_from_slice(&len.to_le_bytes());
        req.0[8..12].copy_from_slice(&c.seqno.get().to_le_bytes());
        // the stream as whole words, aligned
        let words: Vec<u32> = req.0.chunks_exact(4).map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect();
        // SAFETY: the words' bytes.
        let bytes = unsafe { std::slice::from_raw_parts(words.as_ptr() as *const u8, words.len() * 4) };
        self.gpu.submit(c.id, bytes).map_err(|e| -e.0)?;
        if c.shmem_u32(0) != c.seqno.get() {
            return Err(-libc::EIO);
        }
        Ok(())
    }

    fn context_named(&self, id: u32, shmem_res: u32, name: &str) -> Option<Context> {
        self.gpu.ctx_create(id, CAPSET_DRM, name.as_bytes()).ok()?;
        self.gpu.create_blob(id, shmem_res, BLOB_HOST3D, BLOB_FLAG_MAPPABLE, 0, 16384, &[]).ok()?;
        let (offset, _) = self.gpu.map_blob(shmem_res).ok()?;
        // SAFETY: the pool's blob, inside the map.
        let shmem = unsafe { self.ram.add((RAM_BYTES - POOL_BYTES + offset) as usize) };
        let mut c = Context { id, shmem, seqno: Default::default(), slice: 0 };
        // GET_PARAM(VA_START): this context's slice of the VBO
        c.rsp_want(8 + 24);
        let mut req = Req::new(2);
        req.u32(0xc018_6440).u32(0x10).u32(0x0e).u64(0).u32(0).u32(0);
        self.ccmd(&c, req).ok()?;
        c.slice = u64::from_le_bytes(c.rsp_read::<8>(16));
        (c.slice >= self.va_start && c.slice < self.va_start + self.va_size).then_some(c)
    }

    fn context_open(&self, id: u32, shmem_res: u32) -> Option<Context> {
        self.context_named(id, shmem_res, "test")
    }

    fn guest_buffer(
        &self,
        c: &Context,
        res: u32,
        blob: u32,
        iova: u64,
        bytes: u64,
        pages: &[Entry],
    ) -> Result<(), i32> {
        let mut gem = Req::new(3);
        gem.u64(iova).u64(bytes).u32(0x0002_0000 | 0x8000_0000).u32(blob);
        self.ccmd(c, gem)?;
        if c.async_error() != 0 {
            return Err(-libc::EPROTO);
        }
        self.gpu
            .create_blob(c.id, res, BLOB_HOST3D_GUEST, BLOB_FLAG_GUEST_HANDLE, blob as u64, bytes, pages)
            .map_err(|e| -e.0)
    }

    fn set_iova(&self, c: &Context, res: u32, iova: u64) -> Result<(), i32> {
        let mut set = Req::new(4);
        set.u64(iova).u32(res).u32(0);
        self.ccmd(c, set)?;
        if c.async_error() != 0 {
            Err(-libc::EPROTO)
        } else {
            Ok(())
        }
    }

    /// Patch 0008's queue: SUBMITQUEUE_NEW through IOCTL_SIMPLE; its id (0: refused).
    fn queue_new(&self, c: &Context, flags: u32) -> u32 {
        c.rsp_want(8 + 12);
        let mut req = Req::new(2);
        req.u32(0xc00c_644a).u32(flags).u32(1).u32(0);
        if self.ccmd(c, req).is_err() {
            return 0;
        }
        let ret = i32::from_le_bytes(c.rsp_read::<4>(4));
        if ret != 0 {
            0
        } else {
            u32::from_le_bytes(c.rsp_read::<4>(16))
        }
    }

    /// One GEM_SUBMIT on a queue: the guest's resource (bos[0].handle) as the command buffer. MSM_SUBMIT_BO_READ and
    /// MSM_SUBMIT_CMD_BUF are both 1.
    fn submit_one(&self, c: &Context, queue: u32, res: u32, fence: u32) -> Result<(), i32> {
        let mut sub = Req::new(7);
        sub.u32(0).u32(queue).u32(1).u32(1).u32(fence); // flags, queue_id, nr_bos, nr_cmds, fence
        sub.u32(1).u32(res).u64(0); // the bo: flags, handle, presumed
        sub.u32(1).u32(0).u32(0).u32(4).u32(0).u32(0).u64(0); // the cmd: type, submit_idx, offset, size, pad, nr_relocs, relocs
        self.ccmd(c, sub)
    }

    /// An upload of 16 bytes of 0xab at 0x1000 of a resource (MSM_CCMD_GEM_UPLOAD).
    fn upload(&self, c: &Context, res: u32) -> Result<(), i32> {
        let mut up = Req::new(8);
        up.u32(res).u32(0).u32(0x1000).u32(16).bytes(&[0xab; 16]);
        self.ccmd(c, up)
    }

    fn ram_at(&self, offset: usize) -> u8 {
        // SAFETY: inside the map.
        unsafe { self.ram.add(offset).read_volatile() }
    }
}

fn pages(addr: u64, length: u32) -> [Entry; 1] {
    [Entry::new(RAM_PHYS + addr, length)]
}

// ---- the scenarios ---------------------------------------------------------------------------------------------------
#[test]
fn layout_matches_msm_kgsl_h() {
    let c = drm2kgsl_sys::sys::kgsl_layout();
    if c.is_empty() {
        println!("msm_kgsl.h: not checked (no drm2kgsl in this build)");
        return;
    }
    assert_eq!(kgsl::layout(), c);
}

/// What the phone did with runtime 0.1.3: the VBO takes the whole range, and the first import finds no address.
#[test]
fn whole_range() {
    scenario("whole_range", || {
        std::env::set_var("DRM2KGSL_VBO_HEADROOM_GB", "0"); // as drm2kgsl was
        let r = renderer(kgsl::A750);
        assert!(r.va_start == VA_BASE && r.va_size == 256 << 30);
        let c = r.context_open(1, 1).unwrap();
        let res = r.guest_buffer(&c, 2, 1, c.slice + (16 << 20), 0x4000, &pages(1 << 20, 0x4000));
        assert_eq!(res, Err(-libc::ENOMEM));
        assert!(LAST_ERROR.lock().unwrap().contains("GPUOBJ_IMPORT failed: Cannot allocate memory"));
    });
}

#[test]
fn with_room() {
    scenario("with_room", || {
        let r = renderer(kgsl::A750);
        assert!(r.va_start == VA_BASE && r.va_size == 192 << 30); // the next rung: 64 GiB of addresses left
        {
            let s = stand();
            assert!(s.objects.len() == 1 && s.objects[0].vbo); // the room check's reservation is gone
        }
        let c = r.context_open(1, 1).unwrap();
        let vbo = stand().objects[0].id;
        // a small buffer: one udmabuf, one object, one range
        let iova = c.slice + (16 << 20);
        let (before, base) = with(|s| (s.imports, s.binds.len()));
        assert_eq!(r.guest_buffer(&c, 2, 1, iova, 0x4000, &pages(1 << 20, 0x4000)), Ok(()));
        {
            let s = stand();
            let b = s.binds[base];
            assert!(s.imports == before + 1 && s.binds.len() == base + 1 && b.vbo == vbo && b.op == RANGE_OP_BIND);
            assert!(b.target == iova - r.va_start && b.length == 0x4000 && b.child_offset == 0);
        }
        // 72 MiB in one run: two udmabufs (64 + 8 MiB), two objects, bound in a row by one call
        let big_iova = c.slice + (256 << 20);
        let (before, base, calls) = with(|s| (s.imports, s.binds.len(), s.bind_calls));
        assert_eq!(r.guest_buffer(&c, 3, 2, big_iova, 72 << 20, &pages(8 << 20, 72 << 20)), Ok(()));
        let (first_id, second_id) = {
            let s = stand();
            assert!(s.imports == before + 2 && s.binds.len() == base + 2 && s.bind_calls == calls + 1);
            let (a, b) = (s.binds[base], s.binds[base + 1]);
            assert!(
                a.target == big_iova - r.va_start
                    && a.length == 64 << 20
                    && b.target == big_iova - r.va_start + (64 << 20)
            );
            assert!(b.length == 8 << 20 && a.child != b.child && a.child_offset == 0 && b.child_offset == 0);
            assert!(s.find(a.child).unwrap().size == 64 << 20 && s.find(b.child).unwrap().size == 8 << 20);
            (a.child, b.child)
        };
        // 1100 separate pages: two udmabufs (1024 + 76 pages)
        let many: Vec<Entry> = (0..1100).map(|i| Entry::new(RAM_PHYS + (88 << 20) + i * 0x2000, 0x1000)).collect();
        let many_iova = c.slice + (512 << 20);
        let (before, base) = with(|s| (s.imports, s.binds.len()));
        assert_eq!(r.guest_buffer(&c, 4, 3, many_iova, 1100 * 0x1000, &many), Ok(()));
        {
            let s = stand();
            assert!(s.imports == before + 2 && s.binds.len() == base + 2 && s.binds[base].length == 1024 * 0x1000);
            assert!(
                s.binds[base + 1].target == many_iova - r.va_start + 1024 * 0x1000
                    && s.binds[base + 1].length == 76 * 0x1000
            );
        }
        // segments that hold more than the blob: only the blob's bytes are bound (the last chunk's range is cut)
        let more = pages(100 << 20, 0x6000);
        gpu3d::udmabuf_set_limits(0x4000, 4);
        let base = stand().binds.len();
        assert_eq!(r.guest_buffer(&c, 5, 4, c.slice + (768 << 20), 0x5000, &more), Ok(()));
        {
            let s = stand();
            assert!(s.binds.len() == base + 2 && s.binds[base].length == 0x4000 && s.binds[base + 1].length == 0x1000);
        }
        // segments that hold less than the blob: refused, and nothing stays imported
        let live = stand().objects.len();
        assert!(
            r.guest_buffer(&c, 6, 5, c.slice + (800 << 20), 0x9000, &more).is_err() && stand().objects.len() == live
        );
        gpu3d::udmabuf_set_limits(0, 0);

        // a second context attaches the 72 MiB resource: it imports both chunks again and binds them at its own address
        let d = r.context_open(2, 11).unwrap();
        assert_ne!(d.slice, c.slice);
        let (before, base) = with(|s| (s.imports, s.binds.len()));
        assert!(r.gpu.attach(2, 3, true).is_ok() && with(|s| s.imports == before + 2 && s.binds.len() == base));
        assert!(r.gpu.attach(2, 3, true).is_ok() && stand().imports == before + 2); // once
        assert!(r.set_iova(&d, 3, d.slice + (64 << 20)).is_ok());
        {
            let s = stand();
            let (a, b) = (s.binds[base], s.binds[base + 1]);
            assert!(s.binds.len() == base + 2 && a.target == d.slice + (64 << 20) - r.va_start && a.length == 64 << 20);
            assert!(b.target == a.target + (64 << 20) && b.length == 8 << 20);
        }
        // the small one too, by the resource's one fd as before
        let before = stand().imports;
        assert!(r.gpu.attach(2, 2, true).is_ok() && stand().imports == before + 1);

        // gone: every chunk of the creating context is unbound from its slice and freed, the second context's copies are
        // freed
        let (live, base, freed) = with(|s| (s.objects.len(), s.binds.len(), s.frees));
        r.gpu.unref(3);
        {
            let s = stand();
            assert!(s.frees == freed + 4 && s.objects.len() == live - 4 && s.binds_of(RANGE_OP_UNBIND) >= 2);
            let unbound = s.binds[base..]
                .iter()
                .filter(|b| b.op == RANGE_OP_UNBIND && (b.child == first_id || b.child == second_id))
                .count();
            assert_eq!(unbound, 2);
        }
        for res in [4, 5, 2, 1, 11] {
            r.gpu.unref(res);
        }
        r.gpu.ctx_destroy(1);
        r.gpu.ctx_destroy(2);
        let s = stand();
        assert!(s.objects.len() == 1 && s.objects[0].vbo, "{:?}", s.objects); // only the VBO is left
    });
}

/// Patch 0007: each import's flags read back. A guest buffer asks IO coherence and keeps it; on a KGSL that drops the
/// flag (an MMU without IO coherence) the loss is counted and said; an attach by a second context asks none.
#[test]
fn coherence() {
    scenario("coherence", || {
        let r = renderer(kgsl::A750);
        let c = r.context_open(1, 1).unwrap();
        let was = drm2kgsl_sys::import_stats();
        assert_eq!(r.guest_buffer(&c, 2, 1, c.slice + (16 << 20), 0x4000, &pages(1 << 20, 0x4000)), Ok(()));
        let now = drm2kgsl_sys::import_stats();
        assert!(now.imports == was.imports + 1 && now.checked == was.checked + 1 && now.kept == was.kept + 1);
        assert!(now.dropped == was.dropped && now.unreadable == was.unreadable);
        stand().no_io_coherence = true;
        let was = now;
        assert_eq!(r.guest_buffer(&c, 3, 2, c.slice + (32 << 20), 0x4000, &pages(2 << 20, 0x4000)), Ok(()));
        let now = drm2kgsl_sys::import_stats();
        assert!(now.imports == was.imports + 1 && now.dropped == was.dropped + 1 && now.kept == was.kept);
        stand().no_io_coherence = false;
        let _d = r.context_open(2, 11).unwrap();
        let was = drm2kgsl_sys::import_stats();
        assert!(r.gpu.attach(2, 2, true).is_ok());
        let now = drm2kgsl_sys::import_stats();
        assert!(now.imports == was.imports + 1 && now.plain == was.plain + 1);
        for res in [2, 3, 1, 11] {
            r.gpu.unref(res);
        }
        r.gpu.ctx_destroy(1);
        r.gpu.ctx_destroy(2);
    });
}

/// Patch 0008: each queue's KGSL priority is its owner's class (policy.rs), by name or by the guest's hint; a
/// background context's queues can carry KGSL's lowest level; the level setter drives the render server's deadline
/// queue.
#[test]
fn classes() {
    scenario("classes", || {
        let r = renderer(kgsl::A750);
        let render = r.context_named(1, 1, "xnu:backboardd").unwrap();
        let icons = r.context_named(2, 2, "xnu:iconservicesagent").unwrap();
        let app = r.context_named(3, 3, "xnu:MobileSafari").unwrap();
        let other = r.context_named(4, 4, "xnu:MobileSafari").unwrap();
        let photos = r.context_named(5, 5, "xnu:photoanalysisd").unwrap();
        // The guest's hint: Safari in front (role 1), the other one sent to the background (role 2).
        r.gpu.hint(3, [HINT_VALID | ROLE_FOREGROUND, QOS_USER_INTERACTIVE, 0]);
        r.gpu.hint(4, [HINT_VALID | ROLE_BACKGROUND, QOS_USER_INTERACTIVE, 0]);
        let q1 = r.queue_new(&render, 0x8000_0000); // DRM2KGSL_SUBMITQUEUE_DEADLINE: the compositor's
        let (q2, q3, q4, q5) =
            (r.queue_new(&icons, 0), r.queue_new(&app, 0), r.queue_new(&other, 0), r.queue_new(&photos, 0));
        assert!(q1 != 0 && q2 != 0 && q3 != 0 && q4 != 0 && q5 != 0);
        let s = stand();
        assert!(s.priority_of(q1) == 8 && s.constrained(q1));
        // iconservicesagent draws icons the user sees: no longer background by name (fix-gpu-widgets).
        assert!(s.priority_of(q2) == 8 && s.constrained(q2));
        assert!(s.priority_of(q3) == 8 && s.constrained(q3));
        // Background, by role or by name: the default's ringbuffer (never under it), and able to carry the lowest level.
        assert!(s.priority_of(q4) == 8 && s.constrained(q4));
        assert!(s.priority_of(q5) == 8 && s.constrained(q5));
        drop(s);
        // No constraint was set by making them, and the level setter alone sets none (the next submission does).
        drm2kgsl_sys::set_level(false);
        assert!(stand().constraints.is_empty());
        for i in 1..=5 {
            r.gpu.ctx_destroy(i);
        }
    });
}

/// The base S25's Adreno 830: 192 GiB of GPU VA (the S25 Ultra's gpuctl). A 256 GiB VBO leaves no import room, so the
/// renderer must take the 128 GiB rung. The A830 default is WAITTIMESTAMP_CTXTID (patch 0007, the FENCE path
/// segfaulted userspace on this chip); NCTX_NO_WAITTS is the A/B switch back to TIMESTAMP_EVENT(FENCE).
#[test]
fn a830() {
    scenario("a830", || {
        let r = renderer(kgsl::A830);
        assert!(r.va_start == VA_BASE && r.va_size == 128 << 30);
        let c = r.context_open(1, 1).unwrap();
        let q = r.queue_new(&c, 0);
        assert_ne!(q, 0);
        let p = pages(1 << 20, 0x4000);
        assert_eq!(r.guest_buffer(&c, 2, 1, c.slice + (16 << 20), 0x4000, &p), Ok(()));
        let (events, commands) = with(|s| (s.fence_events, s.gpu_commands));
        assert!(
            r.submit_one(&c, q, 2, 1).is_ok() && with(|s| s.gpu_commands == commands + 1 && s.fence_events == events)
        );
        r.gpu.unref(2);
        r.gpu.ctx_destroy(1);

        std::env::set_var("NCTX_NO_WAITTS", "1");
        let c = r.context_open(2, 11).unwrap();
        let q = r.queue_new(&c, 0);
        assert_ne!(q, 0);
        assert_eq!(r.guest_buffer(&c, 3, 2, c.slice + (16 << 20), 0x4000, &p), Ok(()));
        let (events, commands) = with(|s| (s.fence_events, s.gpu_commands));
        assert!(
            r.submit_one(&c, q, 3, 1).is_ok()
                && with(|s| s.gpu_commands == commands + 1 && s.fence_events == events + 1)
        );
        std::env::remove_var("NCTX_NO_WAITTS");
        r.gpu.unref(3);
        r.gpu.ctx_destroy(2);
    });
}

/// A widget renderer that the guest calls non-UI (background by its role) is not background once the render server
/// makes a blob over the same guest pages as one of its blobs (it reads the surface): its next queue is the default's,
/// with no lowest level; a later hint keeps it so. Two contexts of one process sharing pages, or a blob over other
/// pages, change nothing.
#[test]
fn shown_surfaces() {
    scenario("shown_surfaces", || {
        let r = renderer(kgsl::A750);
        let render = r.context_named(1, 1, "xnu:backboardd").unwrap();
        let widget = r.context_named(2, 2, "xnu:WidgetRenderer_Default").unwrap();
        let other = r.context_named(3, 3, "xnu:Extension").unwrap();
        let twin = r.context_named(4, 4, "xnu:Extension").unwrap();
        for id in 2..=4 {
            r.gpu.hint(id, [HINT_VALID | ROLE_NONUI, QOS_USER_INITIATED, 0]);
        }
        assert!(
            last_context().contains("context 4 xnu:Extension: background (by the guest's role"),
            "{}",
            last_context()
        );
        let q1 = r.queue_new(&widget, 0);
        assert!(q1 != 0 && with(|s| s.priority_of(q1) == 8 && s.constrained(q1))); // background by role
                                                                                   // The widget draws into a surface; the extension and its twin (one process) share another; the render server
                                                                                   // imports a page nobody else has, then the widget's.
        let (surface, shared, alone) = (pages(4 << 20, 0x8000), pages(6 << 20, 0x4000), pages(8 << 20, 0x4000));
        assert_eq!(r.guest_buffer(&widget, 10, 1, widget.slice + (16 << 20), 0x8000, &surface), Ok(()));
        assert_eq!(r.guest_buffer(&other, 11, 1, other.slice + (16 << 20), 0x4000, &shared), Ok(()));
        assert_eq!(r.guest_buffer(&twin, 12, 1, twin.slice + (16 << 20), 0x4000, &shared), Ok(()));
        assert_eq!(r.guest_buffer(&render, 13, 1, render.slice + (16 << 20), 0x4000, &alone), Ok(()));
        assert!(!last_context().contains("reads its surfaces"));
        assert_eq!(r.guest_buffer(&render, 14, 2, render.slice + (32 << 20), 0x8000, &surface), Ok(()));
        let want =
            "context 2 xnu:WidgetRenderer_Default: the render server reads its surfaces: default now, not background";
        assert!(last_context().contains(want), "{}", last_context());
        let q2 = r.queue_new(&widget, 0);
        assert!(q2 != 0 && with(|s| s.priority_of(q2) == 8 && s.constrained(q2))); // not background now
                                                                                   // A new hint (a utility thread now) keeps it out of the background.
        r.gpu.hint(2, [HINT_VALID | ROLE_NONUI, QOS_UTILITY, 0]);
        let q3 = r.queue_new(&widget, 0);
        assert!(q3 != 0 && stand().constrained(q3));
        // The extension is still background: its pages are shared with its own twin only.
        let q4 = r.queue_new(&other, 0);
        assert!(q4 != 0 && stand().constrained(q4));
        for res in 10..=14 {
            r.gpu.unref(res);
        }
        for i in 1..=4 {
            r.gpu.ctx_destroy(i);
        }
    });
}

/// Id 0, the key mesa's hash tables reserve: no request that names it may reach a table, where it is an assert that
/// ends xnuhost (boot-jobs, 2026-11-23 13:51: `hash_table_search ... "!key_pointer_is_reserved(ht, key)" failed`, after
/// two refused blobs and two refused unrefs). Every entry point with a resource, context, blob or queue id is called
/// with 0, the device's commands and the guest's msm requests in the command stream both (hosted drm2kgsl patch 0009),
/// and each is refused. Then the same context still makes a buffer, binds it and submits on it, so the renderer lives
/// and its tables are whole.
#[test]
fn zero_ids() {
    scenario("zero_ids", || {
        let r = renderer(kgsl::A750);
        let c = r.context_open(1, 1).unwrap();
        // the device's side: resource 0, context 0
        let p = pages(1 << 20, 0x4000);
        assert!(r.gpu.create_blob(1, 0, BLOB_HOST3D, BLOB_FLAG_MAPPABLE, 0, 16384, &[]).is_err());
        assert!(r.gpu.create_blob(0, 0, BLOB_GUEST, BLOB_FLAG_GUEST_HANDLE, 0, 0x4000, &p).is_err());
        assert!(r.gpu.create_blob(0, 5, BLOB_HOST3D, BLOB_FLAG_MAPPABLE, 0, 16384, &[]).is_err());
        assert!(r.gpu.map_blob(0).is_err());
        assert!(r.gpu.attach(1, 0, true).is_err() && r.gpu.attach(0, 1, true).is_err());
        assert!(r.gpu.attach(1, 0, false).is_err() && r.gpu.attach(0, 1, false).is_err());
        r.gpu.unref(0);
        assert!(r.gpu.ctx_create(0, CAPSET_DRM, b"zero").is_err());
        assert!(r.gpu.submit(0, &[0u8; 16]).is_err() && r.gpu.submit_fence(0, 0, 1).is_err());
        r.gpu.hint(0, [HINT_VALID | ROLE_FOREGROUND, QOS_USER_INTERACTIVE, 0]);
        r.gpu.ctx_destroy(0);
        // a buffer with a GPU address, for the submit on queue 0 below to reach the queue's lookup
        assert_eq!(r.guest_buffer(&c, 2, 1, c.slice + (16 << 20), 0x4000, &p), Ok(()));
        // the guest's side, in the command stream: each request naming resource 0 or queue 0 is answered with an error
        // (their answers differ: an error in the response, the async error, or a refused submit; that the process lives
        // and the context works after them is what counts)
        let _ = r.set_iova(&c, 0, c.slice + (16 << 20));
        let _ = r.upload(&c, 0);
        c.rsp_want(8 + 8);
        let mut prep = Req::new(5); // GEM_CPU_PREP: res_id, op
        prep.u32(0).u32(1);
        let _ = r.ccmd(&c, prep);
        let mut name = Req::new(6); // GEM_SET_NAME: res_id, len, name
        name.u32(0).u32(4).bytes(b"zero");
        let _ = r.ccmd(&c, name);
        let _ = r.submit_one(&c, 0, 2, 1);
        c.rsp_want(8 + 16);
        let mut query = Req::new(9); // SUBMITQUEUE_QUERY: queue_id, param, len
        query.u32(0).u32(0).u32(4);
        let _ = r.ccmd(&c, query);
        c.rsp_want(8 + 8);
        let mut wait = Req::new(10); // WAIT_FENCE: queue_id, fence
        wait.u32(0).u32(1);
        let _ = r.ccmd(&c, wait);
        c.rsp_want(8 + 8);
        let mut close = Req::new(2); // IOCTL_SIMPLE(SUBMITQUEUE_CLOSE) of queue 0
        close.u32(0x4004_644b).u32(0);
        let _ = r.ccmd(&c, close);
        let mut gem = Req::new(3); // GEM_NEW with blob id 0
        gem.u64(c.slice + (32 << 20)).u64(0x4000).u32(0x0002_0000 | 0x8000_0000).u32(0);
        let _ = r.ccmd(&c, gem);
        assert!(r.gpu.create_blob(1, 6, BLOB_HOST3D_GUEST, BLOB_FLAG_GUEST_HANDLE, 0, 0x4000, &p).is_err());
        // and a blob id over 32 bits, which drm_context asserts
        assert!(r.gpu.create_blob(1, 7, BLOB_HOST3D_GUEST, BLOB_FLAG_GUEST_HANDLE, 1 << 32, 0x4000, &p).is_err());
        c.clear_async_error();
        // still alive, and the context still works: a new buffer, a queue, a submit on each buffer
        let q = r.queue_new(&c, 0);
        assert_ne!(q, 0);
        assert_eq!(r.guest_buffer(&c, 3, 2, c.slice + (64 << 20), 0x4000, &pages(2 << 20, 0x4000)), Ok(()));
        assert!(r.submit_one(&c, q, 3, 2).is_ok());
        r.gpu.unref(3);
        assert!(r.submit_one(&c, q, 2, 1).is_ok());
        r.gpu.unref(2);
        r.gpu.unref(1);
        r.gpu.ctx_destroy(1);
        let s = stand();
        assert!(s.objects.len() == 1 && s.objects[0].vbo, "{:?}", s.objects); // only the VBO is left
    });
}

/// What the phone did on 2026-11-23 18:35 (runtime 0.1.18, drm2kgsl 0009): Unity2025Poster's 80 MiB blobs were
/// refused by the device (20480 entries, over its limit then), and xnuhost ended at the end of a poster's context
/// (hash_table.c:311 "!key_pointer_is_reserved"). The guest's GEM_NEW had made the BO with its blob id; the device never
/// passed the CREATE_BLOB on, so the BO had no res_id (0) and stayed in the blob table. At the context's end
/// drm_context_deinit frees it (free_blob), and drm_context_remove_object looked its res_id 0 up in the resource table:
/// 0 is mesa's reserved key. Hosted drm2kgsl patch 0010. Here: the guest's sequence (GEM_NEW, the refused blob, the
/// unref of the refused resource, the destroy's GEM_SET_IOVA of res_id 0 and a submit naming 0), the context's end,
/// and a new context's first submissions; a second refused BO in a context beside another, then both ends.
#[test]
fn refused_blob_then_context_end() {
    scenario("refused_blob_then_context_end", || {
        let r = renderer(kgsl::A750);
        let c = r.context_open(1, 1).unwrap();
        let q = r.queue_new(&c, 0);
        assert_ne!(q, 0);
        assert_eq!(r.guest_buffer(&c, 2, 1, c.slice + (16 << 20), 0x4000, &pages(1 << 20, 0x4000)), Ok(()));
        // the BO of the refused blob: GEM_NEW with blob id 2, and no CREATE_BLOB (the device refused it)
        let mut gem = Req::new(3);
        gem.u64(c.slice + (32 << 20)).u64(80 << 20).u32(0x0002_0000 | 0x8000_0000).u32(2);
        assert_eq!(r.ccmd(&c, gem), Ok(()));
        assert_eq!(c.async_error(), 0);
        r.gpu.unref(3); // the guest's unref of the refused resource (102, answered 1203 by the device)
        let _ = r.set_iova(&c, 0, 0); // the BO's destroy: GEM_SET_IOVA of res_id 0 (patch 0009)
        let _ = r.submit_one(&c, q, 0, 1);
        c.clear_async_error();
        assert!(r.submit_one(&c, q, 2, 2).is_ok()); // the context still works
        r.gpu.unref(2);
        r.gpu.unref(1);
        r.gpu.ctx_destroy(1); // drm_context_deinit: the refused BO is freed from the blob table
                              // a new context's first submissions, and a refused BO in it while another context is open
        let a = r.context_open(4, 4).unwrap();
        let b = r.context_open(5, 5).unwrap();
        let qa = r.queue_new(&a, 0);
        assert_ne!(qa, 0);
        assert_eq!(r.guest_buffer(&a, 6, 1, a.slice + (16 << 20), 0x4000, &pages(2 << 20, 0x4000)), Ok(()));
        assert!(r.submit_one(&a, qa, 6, 1).is_ok());
        let mut gem = Req::new(3);
        gem.u64(a.slice + (32 << 20)).u64(75022336).u32(0x0002_0000 | 0x8000_0000).u32(2);
        assert_eq!(r.ccmd(&a, gem), Ok(()));
        assert_eq!(a.async_error(), 0);
        r.gpu.unref(7);
        assert_eq!(r.guest_buffer(&b, 8, 2, b.slice + (16 << 20), 0x4000, &pages(3 << 20, 0x4000)), Ok(()));
        assert!(r.submit_one(&a, qa, 6, 2).is_ok());
        r.gpu.unref(6);
        r.gpu.unref(4);
        r.gpu.ctx_destroy(4);
        r.gpu.unref(8);
        r.gpu.unref(5);
        r.gpu.ctx_destroy(5);
        let s = stand();
        assert!(s.objects.len() == 1 && s.objects[0].vbo, "{:?}", s.objects); // only the VBO is left
    });
}

// ---- leaks: fds and KGSL objects back to where they were ----------------------------------------------------------------
fn count_fds() -> usize {
    std::fs::read_dir("/proc/self/fd").map_or(0, |d| d.count()) - 1 // the directory's own
}

fn heap_in_use() -> usize {
    // SAFETY: a read of glibc's allocator counters.
    unsafe { libc::mallinfo2() }.uordblks
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Snap {
    fds: usize,
    objs: usize,
}

fn take() -> Snap {
    Snap { fds: count_fds(), objs: stand().live() }
}

fn same(what: &str, a: Snap) -> bool {
    let b = take();
    let ok = a == b;
    println!(
        "  {what:<60} fds {:+} kgsl objects {:+}{}",
        b.fds as i64 - a.fds as i64,
        b.objs as i64 - a.objs as i64,
        if ok { "" } else { "   <-- NOT BACK" }
    );
    ok
}

/// The cycles of test_gpu_leaks.c once: `blob`, `res` the ids to go on from.
fn cycles(r: &Renderer, c: &Context, d: &Context, n: u32, base: Snap, blob: &mut u32, res: &mut u32) {
    let big = pages(8 << 20, 72 << 20);
    let small = pages(1 << 20, 0x4000);
    let many: Vec<Entry> = (0..1100).map(|i| Entry::new(RAM_PHYS + (88 << 20) + i * 0x2000, 0x1000)).collect();
    // 1. context blob -> attach to a second context -> SET_IOVA there -> upload in both -> detach -> unref
    for kind in 0..3 {
        for _ in 0..n {
            *blob += 1;
            *res += 1;
            let (e, bytes, at): (&[Entry], u64, usize) = match kind {
                0 => (&big, 72 << 20, 8 << 20),
                1 => (&small, 0x4000, 1 << 20),
                _ => (&many, 1100 * 0x1000, (88 << 20) + 0x1000),
            };
            assert_eq!(r.guest_buffer(c, *res, *blob, c.slice + (256 << 20), bytes, e), Ok(()));
            assert!(r.upload(c, *res).is_ok());
            assert!(r.gpu.attach(2, *res, true).is_ok());
            assert!(r.set_iova(d, *res, d.slice + (64 << 20)).is_ok());
            assert!(r.upload(d, *res).is_ok());
            assert_eq!(r.ram_at(at + 0x1000), 0xab);
            assert!(r.gpu.attach(2, *res, false).is_ok());
            r.gpu.unref(*res);
        }
        let what =
            ["72 MiB ctx blob, attach/iova/upload/detach/unref", "16 KiB ctx blob, same", "1100-page ctx blob, same"];
        assert!(same(&format!("{n}x {}", what[kind]), base));
    }
    // 2. a display blob (ctx 0, GUEST), chunked and single: attach to both contexts, iova, upload, unref without a detach
    for kind in 0..2 {
        for _ in 0..n {
            *res += 1;
            let (e, bytes) = if kind == 1 { (&small, 0x4000) } else { (&big, 72 << 20) };
            assert!(r.gpu.create_blob(0, *res, BLOB_GUEST, BLOB_FLAG_GUEST_HANDLE, 0, bytes, e).is_ok());
            assert!(r.gpu.attach(1, *res, true).is_ok() && r.gpu.attach(2, *res, true).is_ok());
            assert!(
                r.set_iova(c, *res, c.slice + (512 << 20)).is_ok()
                    && r.set_iova(d, *res, d.slice + (512 << 20)).is_ok()
            );
            assert!(r.upload(c, *res).is_ok() && r.upload(d, *res).is_ok());
            r.gpu.unref(*res);
        }
        let what = if kind == 1 {
            "16 KiB display blob in 2 contexts, unref"
        } else {
            "72 MiB display blob in 2 contexts, unref"
        };
        assert!(same(&format!("{n}x {what}"), base));
    }
    // 3. a context destroyed with resources still attached (its own and others'), then the resources unref'ed
    for _ in 0..n / 3 {
        let e = r.context_open(3, 3).unwrap();
        let (mine, theirs, disp, disp1) = (*res + 1, *res + 2, *res + 3, *res + 4);
        *res += 4;
        *blob += 1;
        assert!(
            r.guest_buffer(&e, mine, *blob, e.slice + (256 << 20), 72 << 20, &big).is_ok()
                && r.upload(&e, mine).is_ok()
        );
        *blob += 1;
        assert!(r.guest_buffer(c, theirs, *blob, c.slice + (256 << 20), 72 << 20, &big).is_ok());
        assert!(r.gpu.create_blob(0, disp, BLOB_GUEST, BLOB_FLAG_GUEST_HANDLE, 0, 72 << 20, &big).is_ok());
        assert!(r.gpu.create_blob(0, disp1, BLOB_GUEST, BLOB_FLAG_GUEST_HANDLE, 0, 0x4000, &small).is_ok());
        assert!(r.gpu.attach(3, theirs, true).is_ok() && r.gpu.attach(3, disp, true).is_ok());
        assert!(r.gpu.attach(3, disp1, true).is_ok() && r.gpu.attach(2, mine, true).is_ok());
        assert!(r.set_iova(&e, theirs, e.slice + (512 << 20)).is_ok() && r.upload(&e, disp).is_ok());
        r.gpu.ctx_destroy(3);
        for x in [mine, theirs, disp, disp1, 3] {
            r.gpu.unref(x);
        }
    }
    assert!(same(&format!("{}x ctx destroyed with own+foreign+display resources attached", n / 3), base));
}

/// The renderer through many create/attach/detach/unref cycles, failed blobs and a reset: every fd and KGSL object
/// back; and a second round of the cycles leaves the heap where the first did (LeakSanitizer's part in the C).
/// CYCLES=N for more cycles.
#[test]
fn leaks() {
    scenario("leaks", || {
        let fd0 = count_fds();
        let n = std::env::var("CYCLES").ok().and_then(|c| c.parse().ok()).unwrap_or(30);
        let r = renderer(kgsl::A750);
        let start = take();
        let c = r.context_open(1, 1).unwrap();
        let d = r.context_open(2, 2).unwrap();
        let base = take();
        let (mut blob, mut res) = (10u32, 100u32);
        cycles(&r, &c, &d, n, base, &mut blob, &mut res);
        let heap = heap_in_use();
        cycles(&r, &c, &d, n, base, &mut blob, &mut res);
        let grown = heap_in_use() as i64 - heap as i64;
        println!("  the heap after a second round of the cycles: {grown:+} bytes");
        assert!(grown < 64 << 10, "the heap grew by {grown} bytes over a round of cycles");
        // 4. a failed get_blob: each kind measured on its own
        let (big, small) = (pages(8 << 20, 72 << 20), pages(1 << 20, 0x4000));
        let s = take();
        res += 1;
        blob += 1;
        assert!(r.guest_buffer(&c, res, blob, 0x1000, 72 << 20, &big).is_err()); // iova outside the slice: bind fails
        assert!(same("chunked blob whose bind fails (iova outside the slice)", s)); // drm2kgsl patch 0003
        let s = take();
        res += 1;
        blob += 1;
        assert!(r.guest_buffer(&c, res, blob, 0x1000, 0x4000, &small).is_err());
        assert!(same("single blob whose bind fails", s)); // drm2kgsl patch 0003
        let s = take();
        res += 1;
        assert!(r.gpu.create_blob(1, res, BLOB_HOST3D_GUEST, BLOB_FLAG_GUEST_HANDLE, 999, 0x4000, &small).is_err()); // no GEM_NEW
        same("single blob with no GEM_NEW for its blob_id (fd parked)", s);
        let s = take();
        res += 1;
        assert!(r.gpu.create_blob(1, res, BLOB_HOST3D_GUEST, BLOB_FLAG_GUEST_HANDLE, 998, 72 << 20, &big).is_err());
        same("chunked blob with no GEM_NEW (chunks parked; replaces the parked single fd?)", s);
        // 5. "reset": every resource, then every context (virtio_gpu.rs teardown's order)
        r.gpu.unref(1);
        r.gpu.unref(2);
        r.gpu.ctx_destroy(1);
        r.gpu.ctx_destroy(2);
        assert!(same("after reset, against before the contexts (parked dma-bufs closed: patch 0003)", start));
        println!("  fds: {fd0} at start, {} after the renderer opened, {} now", start.fds, count_fds());
    });
}
