// SPDX-License-Identifier: MIT
//! test_xnuhost.c's test_virtio_gpu and test_virtio_gpu3d: the 2D device, then the 3D device against a recording
//! renderer, driven by a guest driver and by a generated command stream; guest pages as udmabufs where this host has
//! /dev/udmabuf; the two lanes.
use std::os::fd::{AsFd, AsRawFd};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use xh_core::mmio::MmioDevice;
use xh_core::pixels::{half_from_float, half_to_srgb8};
use xh_core::virtio::testing::{self, Driver, GuestRam};

use super::*;
use crate::gpu3d::{Chunk, Errno};

// ---- what the rest of xnuhost provides --------------------------------------------------------------------------------
/// The display, the ready rule and the display helper, as the device sees them: ready_count counts the frames of the
/// guest's own it was given, last_lit the last one's lit, frame_answer what the session answers (true: ready, the device
/// stops asking), flushes_at_frame the flushes before the last frame came to the rule.
#[derive(Default)]
struct FakeState {
    frame: Option<Frame>,
    buffers: Vec<Box<[u8]>>,
    flush_rect: [u32; 4],
    flushes: i32,
    ready_count: i32,
    last_lit: bool,
    frame_answer: bool,
    flushes_at_frame: i32,
    video: u64,
    direct: bool,
    refuse: bool,
    posts: u32,
    post_entries: Vec<Entry>,
    post_offset: u64,
    post_stride: u32,
    post_size: (u32, u32),
    post_rect: [u32; 4],
    post_seq: u64,
    post_done: Option<Done>,
}

#[derive(Default)]
struct FakeBoard(Mutex<FakeState>);

impl FakeBoard {
    fn st(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.0.lock().unwrap()
    }

    /// A display of `w` x `h` in `format` (0: a display made before formats), all black.
    fn display(&self, w: u32, h: u32, stride: u32, format: u32) {
        let mut s = self.st();
        let mut b = vec![0u8; (stride * h) as usize].into_boxed_slice();
        // SAFETY: the buffer is kept in the fake for the test's life.
        s.frame = Some(unsafe { Frame::new(NonNull::new(b.as_mut_ptr()).unwrap(), b.len(), w, h, stride, format) });
        s.buffers.push(b);
    }

    #[allow(clippy::mut_from_ref)]
    fn pixels(&self) -> &mut [u8] {
        // SAFETY: the test thread reads what the device's lane wrote, after the command was answered.
        unsafe { self.st().frame.unwrap().bytes_mut() }
    }
}

impl Board for FakeBoard {
    fn frame(&self) -> Option<Frame> {
        self.st().frame
    }
    fn flush(&self, x: u32, y: u32, w: u32, h: u32) {
        let mut s = self.st();
        s.flush_rect = [x, y, w, h];
        s.flushes += 1;
    }
    fn direct(&self) -> bool {
        self.st().direct
    }
    fn post(
        &self,
        e: &[Entry],
        offset: u64,
        stride: u32,
        width: u32,
        height: u32,
        r: [u32; 4],
        seq: u64,
        done: &Done,
    ) -> bool {
        let mut s = self.st();
        if s.refuse {
            return false;
        }
        s.post_entries = e.iter().take(4).copied().collect();
        (s.post_offset, s.post_stride, s.post_size, s.post_rect, s.post_seq) =
            (offset, stride, (width, height), r, seq);
        s.post_done = Some(done.clone());
        s.posts += 1;
        true
    }
    fn guest_frame(&self, lit: bool) -> bool {
        let mut s = self.st();
        s.ready_count += 1;
        s.last_lit = lit;
        s.flushes_at_frame = s.flushes;
        s.frame_answer
    }
    fn boot_framebuffer(&self) -> u64 {
        self.st().video
    }
}

/// Guest memory: rings at 0x10000, a command at 0x30000, its response at 0x31000, more of it at 0x32000.
struct Guest {
    ram: GuestRam,
    d: Driver,
    board: Arc<FakeBoard>,
    dev: GpuDev,
}

impl Guest {
    fn pa(&self, off: usize) -> u64 {
        self.ram.pa(off)
    }
    fn cmd(&self, i: usize, v: u32) {
        self.ram.set_u32(0x30000 + 4 * i, v);
    }
    fn cmd_bytes(&self, i: usize, b: &[u8]) {
        self.ram.write(0x30000 + 4 * i, b);
    }
    fn resp(&self, i: usize) -> u32 {
        self.ram.u32(0x31000 + 4 * i)
    }
    fn extra(&self, i: usize, v: u32) {
        self.ram.set_u32(0x32000 + 4 * i, v);
    }
    fn extra_u64s(&self, v: &[u64]) {
        for (i, x) in v.iter().enumerate() {
            self.ram.set_u64(0x32000 + 8 * i, *x);
        }
    }
    fn clear(&self) {
        self.ram.fill(0x30000, 0, 4096);
        self.ram.fill(0x31000, 0xee, 256);
    }
    /// 2D: the command and its response; the used length.
    fn send(&mut self, words: u32, resp_bytes: u32) -> Option<u32> {
        let (c, r) = (self.pa(0x30000), self.pa(0x31000));
        self.d.send(0, &[(c, words * 4, false), (r, resp_bytes, true)], true)
    }
    /// 3D: posts one command (with `extra` bytes in a part of their own) and lets the device's threads finish. How
    /// many commands the device has returned since the last call.
    fn g(&mut self, words: u32, extra: u32, resp_bytes: u32) -> u32 {
        let (c, x, r) = (self.pa(0x30000), self.pa(0x32000), self.pa(0x31000));
        if extra != 0 {
            self.d.send(0, &[(c, words * 4, false), (x, extra, false), (r, resp_bytes, true)], false);
        } else {
            self.d.send(0, &[(c, words * 4, false), (r, resp_bytes, true)], false);
        }
        self.d.kick(0);
        idle(&self.dev);
        self.returned()
    }
    fn returned(&mut self) -> u32 {
        let mut n = 0;
        while self.d.reap(0).is_some() {
            n += 1;
        }
        n
    }
}

fn guest_2d(spi: u32, rings: usize) -> Guest {
    testing::install();
    let ram = GuestRam::new(1 << 20);
    let board = Arc::new(FakeBoard::default());
    board.st().frame_answer = true;
    let dev = create_with(0, spi, 8, 4, None, board.clone()).unwrap();
    let d = Driver::up(dev.clone() as Arc<dyn MmioDevice>, 0, 2, 8, ram.pa(rings)).unwrap();
    Guest { ram, d, board, dev }
}

#[test]
fn two_d() {
    let mut g = guest_2d(42, 0x10000);
    g.board.display(8, 4, 32, BGRX8);
    assert!(g.dev.mmio_read(8, 4) == 16 && g.dev.mmio_read(0x108, 4) == 1);
    g.cmd(0, 0x100); // GET_DISPLAY_INFO
    assert_eq!(g.send(6, 24 + 16 * 24), Some(24 + 16 * 24));
    assert!(g.resp(0) == 0x1101 && g.resp(8) == 8 && g.resp(9) == 4 && g.resp(10) == 1);
    for (i, v) in [(0, 0x101), (6, 1), (7, 67), (8, 8), (9, 4)] {
        g.cmd(i, v); // RESOURCE_CREATE_2D id 1, R8G8B8A8, 8x4
    }
    assert!(g.send(10, 24) == Some(24) && g.resp(0) == 0x1100);
    assert!(g.send(10, 24) == Some(24) && g.resp(0) == 0x1203); // the id is taken
    let px = g.pa(0x40000);
    for (i, v) in [(0, 0x106), (6, 1), (7, 1), (8, px as u32), (9, (px >> 32) as u32), (10, 8 * 4 * 4), (11, 0)] {
        g.cmd(i, v); // ATTACH_BACKING
    }
    assert!(g.send(12, 24) == Some(24) && g.resp(0) == 0x1100);
    for i in 0..32u32 {
        g.ram.set_u32(0x40000 + 4 * i as usize, 0xff00_0000 | i << 16 | 0x80 << 8 | 0x11);
        // R=0x11, G=0x80, B=i, A=0xff
    }
    for (i, v) in [(0, 0x105), (6, 0), (7, 0), (8, 8), (9, 4), (10, 0), (11, 0), (12, 1), (13, 0)] {
        g.cmd(i, v); // TRANSFER_TO_HOST_2D
    }
    assert!(g.send(14, 24) == Some(24) && g.resp(0) == 0x1100);
    g.cmd(0, 0x103);
    g.cmd(10, 0);
    g.cmd(11, 1); // SET_SCANOUT rect, scanout 0, resource 1
    assert!(g.send(12, 24) == Some(24) && g.resp(0) == 0x1100);
    g.cmd(11, 9);
    assert!(g.send(12, 24) == Some(24) && g.resp(0) == 0x1203); // no such resource
    g.cmd(10, 3);
    g.cmd(11, 1);
    assert!(g.send(12, 24) == Some(24) && g.resp(0) == 0x1202); // no such scanout
    for (i, v) in [(0, 0x104), (6, 2), (7, 1), (8, 4), (9, 2), (10, 1), (11, 0), (1, 1), (2, 0x55), (3, 0)] {
        g.cmd(i, v); // RESOURCE_FLUSH (2,1) 4x2, with a fence
    }
    assert!(g.send(12, 24) == Some(24) && g.resp(0) == 0x1100 && g.resp(1) == 1 && g.resp(2) == 0x55);
    g.cmd(1, 0);
    {
        let s = g.board.st();
        assert!(s.flush_rect == [2, 1, 4, 2] && s.ready_count == 1);
    }
    let p = g.board.pixels();
    let px = &p[32 + 2 * 4..]; // pixel (2,1) = index 10: B,G,R,X
    assert!(px[0] == 10 && px[1] == 0x80 && px[2] == 0x11);
    assert!(p[0] == 0 && p[32 + 6 * 4] == 0); // outside the flushed rectangle
    g.cmd(8, 9);
    assert!(g.send(12, 24) == Some(24) && g.resp(0) == 0x1205); // rectangle outside the resource
    g.cmd(0, 0x999);
    assert!(g.send(6, 24) == Some(24) && g.resp(0) == 0x1200);
    g.cmd(0, 0x102);
    g.cmd(6, 1); // RESOURCE_UNREF
    assert!(g.send(8, 24) == Some(24) && g.resp(0) == 0x1100);
    {
        // A half-float 2D resource ('RGhA', 8 bytes a pixel in the backing) onto a half-float display: the rows as
        // they are.
        g.board.display(8, 4, 64, RGBA16F);
        for i in 0..8 * 4 * 4 {
            g.ram.set_u16(0x40000 + 2 * i, (0x3400 + i) as u16);
        }
        g.clear();
        for (i, v) in [(0, 0x101), (6, 5), (7, FMT_RGBA16F), (8, 8), (9, 4)] {
            g.cmd(i, v);
        }
        assert!(g.send(10, 24) == Some(24) && g.resp(0) == 0x1100);
        let px = g.pa(0x40000);
        for (i, v) in [(0, 0x106), (6, 5), (7, 1), (8, px as u32), (9, (px >> 32) as u32), (10, 8 * 4 * 8), (11, 0)] {
            g.cmd(i, v);
        }
        assert!(g.send(12, 24) == Some(24) && g.resp(0) == 0x1100);
        g.clear();
        for (i, v) in [(0, 0x105), (8, 8), (9, 4), (12, 5)] {
            g.cmd(i, v); // TRANSFER_TO_HOST_2D, all of it
        }
        assert!(g.send(14, 24) == Some(24) && g.resp(0) == 0x1100);
        g.clear();
        for (i, v) in [(0, 0x103), (8, 8), (9, 4), (11, 5)] {
            g.cmd(i, v); // SET_SCANOUT
        }
        assert!(g.send(12, 24) == Some(24) && g.resp(0) == 0x1100);
        g.clear();
        for (i, v) in [(0, 0x104), (6, 1), (7, 2), (8, 2), (9, 1), (10, 5)] {
            g.cmd(i, v); // RESOURCE_FLUSH (1,2) 2x1
        }
        assert!(g.send(12, 24) == Some(24) && g.resp(0) == 0x1100);
        let want = g.ram.read(0x40000 + (2 * 8 + 1) * 8, 16);
        let p = g.board.pixels();
        assert!(p[2 * 64 + 8..2 * 64 + 8 + 16] == want[..] && p[0] == 0);
        g.cmd(0, 0x102);
        g.cmd(6, 5);
        assert!(g.send(8, 24) == Some(24) && g.resp(0) == 0x1100);
        g.board.display(8, 4, 32, BGRX8);
        g.clear();
    }
    {
        // The boot framebuffer scanned out by the guest's display driver (the console) is shown but is not the guest's
        // first frame, even lit; the surfaces of its own go to the ready rule, with whether the display's frame is lit,
        // before the frame is flushed to the helper, until the rule answers ready (2026-11-15: backboardd's first frames
        // are black).
        let board = g.board.clone();
        let dev = create_with(0x0, 43, 8, 4, None, board.clone()).unwrap();
        let mut b = Driver::up(dev.clone() as Arc<dyn MmioDevice>, 0, 2, 8, g.pa(0x50000)).unwrap();
        let before = board.st().ready_count;
        board.st().frame_answer = false;
        let (c, r) = (g.pa(0x30000), g.pa(0x31000));
        let mut sendb = |words: u32| b.send(0, &[(c, words * 4, false), (r, 24, true)], true);
        for id in 1..=2u32 {
            let backing = 0x40000 + (id as usize - 1) * 0x1000;
            if id == 1 {
                board.st().video = g.pa(backing); // resource 1 is the boot framebuffer
            }
            g.ram.fill(backing, if id == 1 { 0xff } else { 0 }, 8 * 4 * 4); // the console white, the guest's surface black
            g.clear();
            for (i, v) in [(0, 0x101), (6, id), (7, 2), (8, 8), (9, 4)] {
                g.cmd(i, v); // RESOURCE_CREATE_2D, B8G8R8X8
            }
            assert!(sendb(10) == Some(24) && g.resp(0) == 0x1100);
            let pa = g.pa(backing);
            for (i, v) in
                [(0, 0x106), (6, id), (7, 1), (8, pa as u32), (9, (pa >> 32) as u32), (10, 8 * 4 * 4), (11, 0)]
            {
                g.cmd(i, v);
            }
            assert!(sendb(12) == Some(24) && g.resp(0) == 0x1100);
            g.clear();
            for (i, v) in [(0, 0x105), (8, 8), (9, 4), (12, id)] {
                g.cmd(i, v); // TRANSFER_TO_HOST_2D, all of it
            }
            assert!(sendb(14) == Some(24) && g.resp(0) == 0x1100);
            g.clear();
            for (i, v) in [(0, 0x103), (8, 8), (9, 4), (10, 0), (11, id)] {
                g.cmd(i, v); // SET_SCANOUT
            }
            assert!(sendb(12) == Some(24) && g.resp(0) == 0x1100);
            g.cmd(0, 0x104);
            g.cmd(10, id);
            g.cmd(11, 0); // RESOURCE_FLUSH
            board.st().flush_rect[2] = 0;
            let flushed = board.st().flushes;
            assert!(sendb(12) == Some(24) && g.resp(0) == 0x1100);
            let s = board.st();
            assert!(s.flush_rect[2] == 8 && s.flushes == flushed + 1);
            assert_eq!(s.ready_count, before + (id == 2) as i32);
            drop(s);
            if id == 1 {
                assert_eq!(board.pixels()[0], 0xff); // the console is shown, lit
            } else {
                let s = board.st();
                assert!(!s.last_lit && s.flushes_at_frame == flushed); // black, and asked before the flush
            }
        }
        // The rule said "not yet": the next black frame is asked again; then a lit pixel (at the 8x4 frame's one
        // sample, its middle) is a frame with content; once the rule says ready, no more questions.
        for (i, v) in [(0, 0x104), (6, 0), (7, 0), (8, 8), (9, 4), (10, 2), (11, 0)] {
            g.cmd(i, v);
        }
        assert!(sendb(12) == Some(24) && g.resp(0) == 0x1100);
        assert!(board.st().ready_count == before + 2 && !board.st().last_lit);
        g.ram.write(0x41000 + (2 * 8 + 4) * 4 + 1, &[0x80]); // pixel (4,2): green 0x80
        g.clear();
        for (i, v) in [(0, 0x105), (8, 8), (9, 4), (12, 2)] {
            g.cmd(i, v);
        }
        assert!(sendb(14) == Some(24) && g.resp(0) == 0x1100);
        board.st().frame_answer = true;
        g.clear();
        for (i, v) in [(0, 0x104), (8, 8), (9, 4), (10, 2)] {
            g.cmd(i, v);
        }
        assert!(sendb(12) == Some(24) && g.resp(0) == 0x1100);
        assert!(board.st().ready_count == before + 3 && board.st().last_lit);
        assert!(sendb(12) == Some(24) && g.resp(0) == 0x1100 && board.st().ready_count == before + 3);
        board.st().video = 0;
        g.clear();
    }
    g.d.write(0x70, 0); // reset frees what is left
}

// ---- virtio-gpu 3D: the device against a recording renderer, driven by generated command streams ---------------------
#[derive(Default)]
struct FakeGpu {
    contexts: u32,
    blobs: u32,
    submits: u32,
    unrefs: u32,
    attaches: u32,
    fences: u32,
    live: [u32; 8],
    last: Vec<u8>,
    entries: Vec<Entry>,
    blob_id: u64,
    blob_size: u64,
    blob_mem: u32,
    blob_flags: u32,
    blob_ctx: u32,
    host_blob: Vec<u32>,
    hints: u32,
    hint_ctx: u32,
    hint: [u8; 3],
}

/// The lanes' test: blob 0x5105 (bit 0), 0x5106 (bit 1) made once their bit is 0.
static BLOB_HELD: AtomicI32 = AtomicI32::new(0);
/// The blob held in the fake now (0: none).
static BLOB_IN: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct Fake {
    st: Mutex<FakeGpu>,
    retire: Mutex<Option<gpu3d::Retire>>,
}

impl Fake {
    fn st(&self) -> std::sync::MutexGuard<'_, FakeGpu> {
        self.st.lock().unwrap()
    }
    fn retire(&self, ctx: u32, ring: u32, fence: u64) {
        let r = self.retire.lock().unwrap().clone().unwrap();
        r(ctx, ring, fence)
    }
}

impl Gpu3d for Arc<Fake> {
    fn capset_count(&self) -> u32 {
        1
    }
    fn capset_info(&self, i: u32) -> Option<(u32, u32, u32)> {
        (i == 0).then_some((6, 0, 64))
    }
    fn capset(&self, id: u32, _version: u32, out: &mut [u8]) -> usize {
        if id != 6 || out.len() < 64 {
            return 0;
        }
        for (i, b) in out[..64].iter_mut().enumerate() {
            *b = 0xc0 + i as u8;
        }
        64
    }
    fn ctx_create(&self, ctx: u32, capset: u32, name: &[u8]) -> Result<(), Errno> {
        if capset != 6 || ctx == 99 {
            return Err(Errno(libc::ENOMEM));
        }
        if name != b"test" && name != b"xnu:backboardd" {
            return Err(Errno(libc::EINVAL));
        }
        let mut s = self.st();
        let Some(slot) = s.live.iter().position(|&c| c == 0) else { return Err(Errno(libc::ENOMEM)) };
        s.live[slot] = ctx;
        s.contexts += 1;
        Ok(())
    }
    fn ctx_destroy(&self, ctx: u32) {
        let mut s = self.st();
        for i in 0..8 {
            if s.live[i] == ctx {
                s.live[i] = 0;
                s.contexts -= 1;
            }
        }
    }
    fn submit(&self, _ctx: u32, commands: &[u8]) -> Result<(), Errno> {
        if commands.len() >= 4 && commands[..4] == [0xad, 0xde, 0, 0] {
            return Err(Errno(libc::EINVAL));
        }
        let mut s = self.st();
        s.submits += 1;
        s.last = commands[..commands.len().min(256)].to_vec();
        Ok(())
    }
    /// Ring 0 is retired inside the call (the host's own timeline); ring 9 is refused; the others wait for the test.
    fn submit_fence(&self, ctx: u32, ring: u32, fence: u64) -> Result<(), Errno> {
        self.st().fences += 1;
        if ring == 9 {
            return Err(Errno(libc::EINVAL));
        }
        if ring == 0 {
            self.retire(ctx, ring, fence);
        }
        Ok(())
    }
    fn create_blob(
        &self,
        ctx: u32,
        res: u32,
        mem: u32,
        flags: u32,
        blob_id: u64,
        size: u64,
        e: &[Entry],
    ) -> Result<(), Errno> {
        if blob_id == 0xbad {
            return Err(Errno(libc::EINVAL));
        }
        while (blob_id == 0x5105 || blob_id == 0x5106)
            && BLOB_HELD.load(Ordering::Acquire) & (1 << (blob_id - 0x5105)) != 0
        {
            BLOB_IN.store(blob_id, Ordering::Release);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        BLOB_IN.store(0, Ordering::Release);
        let mut s = self.st();
        s.blobs += 1;
        (s.blob_ctx, s.blob_mem, s.blob_flags, s.blob_id, s.blob_size) = (ctx, mem, flags, blob_id, size);
        s.entries = e.to_vec();
        if mem == gpu3d::BLOB_HOST3D && s.host_blob.len() < 4 {
            s.host_blob.push(res);
        }
        Ok(())
    }
    fn map_blob(&self, res: u32) -> Result<(u64, u32), Errno> {
        let s = self.st();
        let i = s.host_blob.iter().position(|&r| r == res).ok_or(Errno(libc::EINVAL))?;
        Ok((0x200000 + i as u64 * 0x4000, gpu3d::MAP_CACHE_CACHED))
    }
    fn attach(&self, _ctx: u32, _res: u32, on: bool) -> Result<(), Errno> {
        self.st().attaches += if on { 1 } else { 100 };
        Ok(())
    }
    fn unref(&self, _res: u32) {
        let mut s = self.st();
        s.unrefs += 1;
        s.blobs -= 1;
    }
    fn hint(&self, ctx: u32, h: [u8; 3]) {
        let mut s = self.st();
        s.hints += 1;
        s.hint_ctx = ctx;
        s.hint = h;
    }
    fn set_retire(&self, retire: gpu3d::Retire) {
        *self.retire.lock().unwrap() = Some(retire);
    }
}

/// The generated stream's numbers: a linear congruential generator.
struct Rnd(u32);

impl Rnd {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
        self.0 >> 8
    }
}

/// A stand-in for UDMABUF_CREATE_LIST that refuses lists of more than 16 MiB (REFUSE_ERRNO, ENOMEM by default), as a
/// kernel whose udmabuf page array is one kmalloc does under fragmentation. Counts the refusals.
static REFUSE_ERRNO: AtomicI32 = AtomicI32::new(libc::ENOMEM);
static REFUSED: AtomicI32 = AtomicI32::new(0);
fn refuse_over_16(dev: i32, list: &mut gpu3d::UdmabufList) -> libc::c_int {
    let total: u64 = list.items().map(|(_, _, size)| size).sum();
    if total > 16 << 20 {
        REFUSED.fetch_add(1, Ordering::Relaxed);
        xh_core::sys::set_errno(REFUSE_ERRNO.load(Ordering::Relaxed));
        return -1;
    }
    list.ioctl(dev)
}

/// The chunks of a blob over a memfd whose page N begins with the byte N: `want` chunks of pages[i] pages, the first
/// page of each being first[i] and the following ones `step` pages apart.
fn chunks_are(chunks: Result<Vec<Chunk>, Errno>, pages: &[u64], first: &[u8], step: u8) -> bool {
    let Ok(chunks) = chunks else { return false };
    chunks.len() == pages.len()
        && chunks.iter().zip(pages).zip(first).all(|((c, &n), &f)| {
            let size = n * 0x1000;
            if c.size != size || seek_end(c.fd.as_raw_fd()) != size as i64 {
                return false;
            }
            let Ok(map) = oneios_display::sys::Map::new(c.fd.as_fd(), size as usize, false) else { return false };
            (0..n as usize).all(|p| map.bytes()[p * 0x1000] == f.wrapping_add(p as u8 * step))
        })
}

fn seek_end(fd: i32) -> i64 {
    // SAFETY: an lseek on our descriptor.
    unsafe { libc::lseek(fd, 0, libc::SEEK_END) }
}

#[test]
fn three_d() {
    testing::install();
    let ram = GuestRam::new(1 << 20);
    let fake = Arc::new(Fake::default());
    let board = Arc::new(FakeBoard::default());
    board.st().frame_answer = true;
    board.display(8, 4, 32, 0);
    let dev = create_with(0, 43, 8, 4, Some(Box::new(fake.clone())), board.clone()).unwrap();
    assert!(fake.retire.lock().unwrap().is_some());
    dev.mmio_write(0x14, 4, 0);
    // VIRGL, BLOB, CONTEXT_INIT, GUEST_HANDLE; one capset
    assert!(dev.mmio_read(8, 4) == 16 && dev.mmio_read(0x10, 4) & 0x59 == 0x59);
    assert!(dev.mmio_read(0x108, 4) == 1 && dev.mmio_read(0x10c, 4) == 1);
    let d = Driver::up(dev.clone() as Arc<dyn MmioDevice>, 0x59, 2, 8, ram.pa(0x10000)).unwrap();
    let mut g = Guest { ram, d, board: board.clone(), dev: dev.clone() };
    let st = || fake.st();
    // capsets
    g.clear();
    g.cmd(0, 0x108);
    g.cmd(6, 0);
    assert!(g.g(8, 0, 40) == 1 && g.resp(0) == 0x1102 && g.resp(6) == 6 && g.resp(7) == 0 && g.resp(8) == 64);
    g.cmd(6, 1);
    assert!(g.g(8, 0, 40) == 1 && g.resp(0) == 0x1102 && g.resp(6) == 0 && g.resp(8) == 0); // past the last: zeros
    g.clear();
    g.cmd(0, 0x109);
    g.cmd(6, 6);
    assert!(g.g(8, 0, 24 + 64) == 1 && g.resp(0) == 0x1103 && g.ram.read(0x31000 + 24, 1)[0] == 0xc0);
    assert_eq!(g.ram.read(0x31000 + 87, 1)[0], 0xff);
    g.cmd(6, 2);
    assert!(g.g(8, 0, 24 + 64) == 1 && g.resp(0) == 0x1205); // no such capset
                                                             // contexts. context_init: the capset, and the creating task's hint in bits 8-23 (OneiOSAdreno.cpp oneios_gpu_hint)
    g.clear();
    for (i, v) in [(0, 0x200), (4, 5), (6, 4), (7, 6 | 0x81 << 8 | 6 << 16)] {
        g.cmd(i, v);
    }
    g.cmd_bytes(8, b"test");
    assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1100 && st().contexts == 1 && st().live[0] == 5);
    assert!(st().hints == 1 && st().hint_ctx == 5 && st().hint == [0x81, 6, 0]);
    assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1204 && st().contexts == 1); // the id is taken
    g.cmd(4, 0);
    assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1204);
    g.cmd(4, 99);
    assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1201 && st().contexts == 1); // the renderer has no memory
    g.cmd(4, 6);
    assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1100 && st().contexts == 2);
    // a context's shmem: a HOST3D blob, mapped from the pool
    g.clear();
    for (i, v) in [(0, 0x10c), (4, 5), (6, 10), (7, 2), (8, 1), (9, 0), (10, 0), (12, 16384)] {
        g.cmd(i, v);
    }
    assert!(g.g(14, 0, 24) == 1 && g.resp(0) == 0x1100);
    assert!(
        st().blobs == 1
            && st().blob_ctx == 5
            && st().blob_mem == 2
            && st().blob_size == 16384
            && st().entries.is_empty()
    );
    assert!(g.g(14, 0, 24) == 1 && g.resp(0) == 0x1203 && st().blobs == 1); // the id is taken
    g.cmd(6, 12);
    g.cmd(4, 7);
    assert!(g.g(14, 0, 24) == 1 && g.resp(0) == 0x1204); // no such context
    g.cmd(4, 5);
    g.cmd(9, 1);
    assert!(g.g(14, 16, 24) == 1 && g.resp(0) == 0x1205); // a host blob has no guest pages
    g.clear();
    for (i, v) in [(0, 0x208), (4, 5), (6, 10)] {
        g.cmd(i, v);
    }
    // MAP_INFO_POOL | cached, pool offset
    assert!(g.g(10, 0, 32) == 1 && g.resp(0) == 0x1106 && g.resp(6) == (0x8000_0000 | 1) && g.resp(7) == 0x200000);
    g.cmd(6, 77);
    assert!(g.g(10, 0, 32) == 1 && g.resp(0) == 0x1203);
    // a BO: the guest's pages (HOST3D_GUEST + CREATE_GUEST_HANDLE), two segments
    g.clear();
    for (i, v) in [(0, 0x10c), (4, 5), (6, 11), (7, 3), (8, 9), (9, 2), (10, 7), (12, 0x3000)] {
        g.cmd(i, v);
    }
    let mut segs = [g.pa(0x50000), 0x2000, g.pa(0x60000), 0x1000];
    g.extra_u64s(&segs);
    assert!(g.g(14, 32, 24) == 1 && g.resp(0) == 0x1100);
    assert!(
        st().blobs == 2 && st().blob_mem == 3 && st().blob_flags == 9 && st().blob_id == 7 && st().entries.len() == 2
    );
    assert!(
        st().entries[0] == Entry::new(g.pa(0x50000), 0x2000) && st().entries[1] == Entry::new(g.pa(0x60000), 0x1000)
    );
    g.clear();
    for (i, v) in [(0, 0x208), (4, 5), (6, 11)] {
        g.cmd(i, v);
    }
    assert!(g.g(10, 0, 32) == 1 && g.resp(0) == 0x1205); // the guest's own pages are not in the pool
    g.clear();
    for (i, v) in [(0, 0x10c), (4, 5), (6, 13), (7, 3), (8, 9), (9, 2), (10, 8), (12, 0x3000)] {
        g.cmd(i, v);
    }
    segs[1] = 0x1800;
    g.extra_u64s(&segs);
    assert!(g.g(14, 32, 24) == 1 && g.resp(0) == 0x1205); // not whole pages
    segs[1] = 0x2000;
    segs[2] = 0x1000;
    g.extra_u64s(&segs);
    // not guest RAM, and the refusal's line says so
    let (refused, lines) = xh_core::log::capture(|| g.g(14, 32, 24) == 1 && g.resp(0) == 0x1205);
    assert!(refused && lines.iter().any(|(_, l)| l.contains("refused") && l.contains("is not guest RAM")));
    segs[2] = g.pa(0x60000);
    g.cmd(12, 0x4000);
    g.extra_u64s(&segs);
    assert!(g.g(14, 32, 24) == 1 && g.resp(0) == 0x1205); // smaller than the blob
    g.cmd(12, 0x3000);
    assert!(g.g(14, 16, 24) == 1 && g.resp(0) == 0x1205); // fewer segments than it says
    g.cmd(10, 0xbad);
    assert!(g.g(14, 32, 24) == 1 && g.resp(0) == 0x1200 && st().blobs == 2); // the renderer refuses
    g.cmd(10, 8);
    assert!(g.g(14, 32, 24) == 1 && g.resp(0) == 0x1100 && st().blobs == 3); // and the id is still free
                                                                             // attach, detach
    g.clear();
    for (i, v) in [(0, 0x202), (4, 6), (6, 11)] {
        g.cmd(i, v);
    }
    assert!(g.g(8, 0, 24) == 1 && g.resp(0) == 0x1100 && st().attaches == 1);
    g.cmd(0, 0x203);
    assert!(g.g(8, 0, 24) == 1 && g.resp(0) == 0x1100 && st().attaches == 101);
    g.cmd(6, 55);
    assert!(g.g(8, 0, 24) == 1 && g.resp(0) == 0x1203);
    // SUBMIT_3D: the stream reaches the renderer whole, also when it is a part of its own in the chain, and the
    // header's three padding bytes (the submitting task's hint: role, QoS, deadline) reach it first
    g.clear();
    for (i, v) in [
        (0, 0x207),
        (4, 5),
        (5, (0x8a | 5 << 8 | 33 << 16) << 8),
        (6, 12),
        (8, 0x1111_1111),
        (9, 0x2222_2222),
        (10, 0x3333_3333),
    ] {
        g.cmd(i, v);
    }
    let hints = st().hints;
    assert!(g.g(11, 0, 24) == 1 && g.resp(0) == 0x1100 && g.resp(1) == 0 && st().submits == 1);
    assert!(st().last.len() == 12 && st().last[..] == g.ram.read(0x30000 + 32, 12)[..]);
    assert!(st().hints == hints + 1 && st().hint_ctx == 5 && st().hint == [0x8a, 5, 33]);
    for i in 0..50u32 {
        g.extra(i as usize, i.wrapping_mul(0x0101_0101));
    }
    g.cmd(6, 200);
    assert!(
        g.g(8, 200, 24) == 1
            && g.resp(0) == 0x1100
            && st().submits == 2
            && st().last[..] == g.ram.read(0x32000, 200)[..]
    );
    g.cmd(6, 204);
    assert!(g.g(8, 200, 24) == 1 && g.resp(0) == 0x1205 && st().submits == 2); // longer than the chain
    g.cmd(6, 0);
    assert!(g.g(8, 0, 24) == 1 && g.resp(0) == 0x1205);
    g.cmd(6, 8);
    g.cmd(8, 0xdead);
    assert!(g.g(10, 0, 24) == 1 && g.resp(0) == 0x1200 && st().submits == 2); // the renderer refuses the stream
    g.cmd(4, 8);
    g.cmd(8, 1);
    assert!(g.g(10, 0, 24) == 1 && g.resp(0) == 0x1204);
    // fences. On the context's ring 1: the command stays with the device until the ring passes the fence.
    g.clear();
    for (i, v) in [(0, 0x207), (1, 3), (2, 1), (4, 5), (5, 1), (6, 4), (8, 0xaa)] {
        g.cmd(i, v);
    }
    let before = g.d.used_idx(0);
    assert!(g.g(9, 0, 24) == 0 && st().submits == 3 && st().fences == 1 && g.d.used_idx(0) == before);
    // the response is written, the chain is held
    assert!(g.resp(0) == 0x1100 && g.resp(1) == 3 && g.resp(2) == 1 && g.resp(4) == 5 && g.resp(5) & 0xff == 1);
    g.cmd(2, 2);
    assert!(g.g(9, 0, 24) == 0 && st().submits == 4);
    g.cmd(2, 3);
    assert!(g.g(9, 0, 24) == 0 && st().submits == 5);
    g.cmd(1, 0);
    g.cmd(2, 0);
    assert!(g.g(9, 0, 24) == 1 && st().submits == 6); // an unfenced one passes the held ones
    fake.retire(6, 1, 3); // another context's ring: nothing
    fake.retire(5, 2, 3); // another ring: nothing
    assert_eq!(g.returned(), 0);
    fake.retire(5, 1, 2); // fences 1 and 2 (the renderer may name only the latest)
    assert_eq!(g.returned(), 2);
    fake.retire(5, 1, 3);
    assert!(g.returned() == 1 && dev.mmio_read(0x60, 4) == 1);
    g.cmd(1, 3);
    g.cmd(2, 9);
    g.cmd(5, 0);
    assert!(g.g(9, 0, 24) == 1 && g.resp(2) == 9); // ring 0: retired inside the call
    g.cmd(5, 9);
    assert_eq!(g.g(9, 0, 24), 1); // a fence the renderer refuses is not held
    g.cmd(1, 1);
    g.cmd(2, 0x77);
    g.cmd(5, 0);
    // the device's own ring: at once, the renderer not asked
    assert!(g.g(9, 0, 24) == 1 && g.resp(1) == 1 && g.resp(2) == 0x77 && st().fences == 5);
    // a fenced command of a context that is destroyed comes back
    g.cmd(1, 3);
    g.cmd(2, 4);
    g.cmd(5, 1);
    assert_eq!(g.g(9, 0, 24), 0);
    g.clear();
    g.cmd(0, 0x201);
    g.cmd(4, 5);
    assert!(g.g(6, 0, 24) == 2 && g.resp(0) == 0x1100 && st().contexts == 1 && st().live[0] == 0);
    assert!(g.g(6, 0, 24) == 1 && g.resp(0) == 0x1204);
    // the display's surface: a guest blob as the scanout, its rows copied at the flush
    g.clear();
    for (i, v) in [(0, 0x10c), (6, 20), (7, 1), (8, 8), (9, 1), (12, 0x1000)] {
        g.cmd(i, v);
    }
    g.extra_u64s(&[g.pa(0x40000), 0x1000]);
    assert!(g.g(14, 16, 24) == 1 && g.resp(0) == 0x1100 && st().blob_mem == 1 && st().blob_ctx == 0);
    for i in 0..40u32 {
        g.ram.set_u32(0x40000 + 4 * i as usize, 0x0010_2000 | i); // BGRX, stride 40: B = index
    }
    let scanout = |g: &Guest, res: u32, format: u32, stride: u32| {
        g.clear();
        for (i, v) in [(0, 0x10d), (8, 8), (9, 4), (10, 0), (11, res), (12, 8), (13, 4), (14, format), (16, stride)] {
            g.cmd(i, v);
        }
    };
    scanout(&g, 20, 2, 40);
    assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1100);
    g.cmd(16, 16);
    assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1205); // a stride shorter than a row
    g.cmd(16, 40);
    g.cmd(13, 200);
    assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1205); // more rows than the blob holds
    g.cmd(13, 4);
    g.cmd(11, 11);
    assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1100); // any guest blob can be one
    g.cmd(11, 10);
    assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1203); // not the renderer's own
    g.cmd(11, 20);
    assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1100);
    board.st().frame_answer = false; // not ready yet: the next frames are asked about too
    g.clear();
    for (i, v) in [(0, 0x104), (1, 1), (2, 5), (6, 1), (7, 2), (8, 3), (9, 2), (10, 20)] {
        g.cmd(i, v); // flush (1,2) 3x2, fenced
    }
    assert!(g.g(12, 0, 24) == 1 && g.resp(0) == 0x1100 && g.resp(2) == 5 && board.st().ready_count == 1);
    assert_eq!(board.st().flush_rect, [1, 2, 3, 2]);
    let p = board.pixels();
    assert!(p[2 * 32 + 4] == 21 && p[2 * 32 + 5] == 0x20 && p[2 * 32 + 6] == 0x10 && p[3 * 32 + 3 * 4] == 33);
    assert!(p[0] == 0 && p[2 * 32 + 4 * 4] == 0);
    // a helper that copies the frame itself: the flush is handed over, not copied here, and answered when it says so
    board.st().direct = true;
    board.pixels()[..4 * 32].fill(0);
    board.st().flush_rect[2] = 0;
    g.ram.set_u32(0x40000 + (2 * 10 + 4) * 4, 0); // the one pixel an 8x4 frame is sampled at: black
    g.clear();
    for (i, v) in [(0, 0x104), (6, 0), (7, 1), (8, 8), (9, 3), (10, 20)] {
        g.cmd(i, v); // (0,1) 8x3
    }
    assert!(g.g(12, 0, 24) == 0 && board.st().posts == 1 && board.st().flush_rect[2] == 0 && board.pixels()[32] == 0);
    // ... and whether it is lit is read in the guest's pages, not in the display's buffer (never copied to: black)
    assert!(board.st().ready_count == 2 && !board.st().last_lit);
    g.ram.set_u32(0x40000 + (2 * 10 + 4) * 4, 0x0010_2000 | 24);
    {
        let s = board.st();
        assert!(
            s.post_entries.len() == 1
                && s.post_entries[0].addr == g.pa(0x40000)
                && s.post_stride == 40
                && s.post_offset == 0
        );
        assert!(s.post_size == (8, 4) && s.post_rect == [0, 1, 8, 3]);
    }
    let first_seq = board.st().post_seq;
    g.clear();
    for (i, v) in [(0, 0x104), (8, 8), (9, 4), (10, 20)] {
        g.cmd(i, v);
    }
    assert!(g.g(12, 0, 24) == 0 && board.st().posts == 2 && board.st().post_seq == first_seq + 1); // a second, held too
    assert!(board.st().ready_count == 3 && board.st().last_lit); // lit in the guest's pages
    board.st().frame_answer = true;
    let done = board.st().post_done.clone().unwrap();
    done(first_seq);
    assert_eq!(g.returned(), 1); // the first answered
    let last = board.st().post_seq;
    done(last);
    assert!(g.returned() == 1 && g.resp(0) == 0x1100);
    done(last); // twice: nothing
    assert_eq!(g.returned(), 0);
    board.st().refuse = true; // the helper went: copied here
    g.clear();
    for (i, v) in [(0, 0x104), (8, 8), (9, 4), (10, 20)] {
        g.cmd(i, v);
    }
    assert!(
        g.g(12, 0, 24) == 1
            && g.resp(0) == 0x1100
            && board.st().flush_rect[2] == 8
            && board.pixels()[3 * 32 + 3 * 4] == 33
    );
    (board.st().direct, board.st().refuse) = (false, false);
    {
        // Half floats (display-format=rgba16f, oneios-display-wide=1): a 'RGhA' scanout's rows go to a half-float
        // display as they are, and a B,G,R,X one is decoded to linear light; onto the 8-bit display half floats are
        // encoded and clamped.
        board.display(8, 4, 64, RGBA16F);
        for i in 0..8 * 4 * 4 {
            g.ram.set_u16(0x40000 + 2 * i, (0x3000 + i) as u16);
        }
        g.ram.set_u16(0x40000 + (8 + 2) * 8, half_from_float(1.2249)); // pixel (2,1): Display P3's red, outside sRGB
        g.ram.set_u16(0x40000 + (8 + 2) * 8 + 2, half_from_float(-0.042));
        scanout(&g, 20, FMT_RGBA16F, 64);
        assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1100);
        g.cmd(16, 60);
        assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1205); // not whole half-float pixels
        g.cmd(16, 32);
        assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1205); // an 8-bit stride: a row does not fit
        g.cmd(16, 64);
        g.cmd(14, 99);
        assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1205); // an unknown format
        g.cmd(14, FMT_RGBA16F);
        assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1100);
        g.clear();
        for (i, v) in [(0, 0x104), (6, 1), (7, 1), (8, 3), (9, 2), (10, 20)] {
            g.cmd(i, v); // flush (1,1) 3x2
        }
        assert!(
            g.g(12, 0, 24) == 1
                && g.resp(0) == 0x1100
                && board.st().flush_rect[0] == 1
                && board.st().flush_rect[2] == 3
        );
        let p = board.pixels();
        let half = |b: &[u8], i: usize| u16::from_le_bytes([b[2 * i], b[2 * i + 1]]);
        let got = &p[64 + 2 * 8..];
        assert!(half(got, 0) == half_from_float(1.2249) && half(got, 1) == half_from_float(-0.042));
        assert_eq!(half(got, 2), g.ram.u16(0x40000 + (8 + 2) * 8 + 4));
        assert!(p[2 * 64 + 3 * 8..2 * 64 + 4 * 8] == g.ram.read(0x40000 + (2 * 8 + 3) * 8, 8)[..] && p[0] == 0);
        // a B,G,R,X scanout onto the half-float display: decoded
        for i in 0..40 {
            g.ram.set_u32(0x40000 + 4 * i, 0x00ff_8000); // B 0, G 128, R 255
        }
        scanout(&g, 20, 2, 40);
        assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1100);
        g.clear();
        for (i, v) in [(0, 0x104), (6, 0), (7, 0), (8, 1), (9, 1), (10, 20)] {
            g.cmd(i, v);
        }
        assert!(g.g(12, 0, 24) == 1 && g.resp(0) == 0x1100);
        let p = board.pixels();
        assert!(
            half(p, 0) == 0x3c00
                && half(p, 1) == half_from_float(0.215_860_5)
                && half(p, 2) == 0
                && half(p, 3) == 0x3c00
        );
        // a pixel split between two segments of a blob: rows at offset 4060 put pixel 4 across the page boundary
        g.clear();
        for (i, v) in [(0, 0x10c), (6, 30), (7, 1), (8, 8), (9, 2), (12, 0x2000)] {
            g.cmd(i, v);
        }
        g.extra_u64s(&[g.pa(0x42000), 0x1000, g.pa(0x44000), 0x1000]);
        assert!(g.g(14, 32, 24) == 1 && g.resp(0) == 0x1100);
        let want: Vec<u8> = (0..32u16).flat_map(|i| (0x3800 + i).to_le_bytes()).collect();
        for (b, &byte) in want.iter().enumerate() {
            let at = 4060 + b;
            g.ram.write(if at < 4096 { 0x42000 + at } else { 0x44000 + at - 4096 }, &[byte]);
        }
        g.clear();
        for (i, v) in
            [(0, 0x10d), (8, 8), (9, 1), (10, 0), (11, 30), (12, 8), (13, 1), (14, FMT_RGBA16F), (16, 64), (20, 4060)]
        {
            g.cmd(i, v);
        }
        assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1100);
        g.clear();
        for (i, v) in [(0, 0x104), (6, 0), (7, 0), (8, 8), (9, 1), (10, 30)] {
            g.cmd(i, v);
        }
        assert!(g.g(12, 0, 24) == 1 && g.resp(0) == 0x1100);
        assert_eq!(board.pixels()[..64], want[..]);
        // half floats onto the 8-bit display: encoded to sRGB, clamped
        board.display(8, 4, 32, BGRX8);
        assert!(g.g(12, 0, 24) == 1 && g.resp(0) == 0x1100); // the same flush
        let eight = board.pixels();
        assert!(eight[0] == 188 && eight[3] == 255); // 0.5 is sRGB 188; X opaque
        let t = half_to_srgb8();
        for p in 0..8 {
            let w = |c: usize| t[0x3800 + p * 4 + c];
            assert!(
                eight[p * 4] == w(2) && eight[p * 4 + 1] == w(1) && eight[p * 4 + 2] == w(0),
                "half floats encoded to B,G,R,X"
            );
        }
        board.display(8, 4, 32, 0);
        scanout(&g, 20, 2, 40);
        assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1100); // back to the B,G,R,X scanout
        for i in 0..40u32 {
            g.ram.set_u32(0x40000 + 4 * i as usize, 0x0010_2000 | i);
        }
    }
    // the 2D commands stay
    g.clear();
    g.cmd(0, 0x100);
    assert!(g.g(6, 0, 24 + 16 * 24) == 1 && g.resp(0) == 0x1101 && g.resp(8) == 8 && g.resp(9) == 4);
    for (i, v) in [(0, 0x101), (6, 20), (7, 67), (8, 8), (9, 4)] {
        g.cmd(i, v);
    }
    assert!(g.g(10, 0, 24) == 1 && g.resp(0) == 0x1203); // a blob has the id
    g.cmd(6, 1);
    assert!(g.g(10, 0, 24) == 1 && g.resp(0) == 0x1100);
    // unref
    g.clear();
    g.cmd(0, 0x102);
    g.cmd(6, 11);
    let blobs = st().blobs;
    assert!(g.g(8, 0, 24) == 1 && g.resp(0) == 0x1100 && st().blobs == blobs - 1);
    assert!(g.g(8, 0, 24) == 1 && g.resp(0) == 0x1203);
    g.cmd(6, 1);
    assert!(g.g(8, 0, 24) == 1 && g.resp(0) == 0x1100); // the 2D resource

    // A generated stream: 4000 commands of every type and of none, with lengths, ids and flags drawn at random. The
    // device answers each one in order with a response or an error, never touches memory it should not, and its
    // contexts stay the renderer's.
    let seed =
        std::env::var("XNUHOST_TEST_SEED").ok().and_then(|s| xh_core::parse::u64_text(&s, 0)).unwrap_or(0x1234567);
    let mut rnd = Rnd(seed as u32);
    let types = [
        0x100, 0x101, 0x102, 0x103, 0x104, 0x105, 0x106, 0x107, 0x108, 0x109, 0x10a, 0x10c, 0x10d, 0x200, 0x201, 0x202,
        0x203, 0x207, 0x208, 0x209, 0x300, 0x999,
    ];
    let (mut answered, mut errors, mut held) = (0, 0, 0);
    for i in 0..4000 {
        for w in 0..40 {
            let r = rnd.next();
            g.cmd(w, if r % 7 != 0 { rnd.next() % 24 } else { r << 8 });
        }
        for w in 0..64 {
            let (r, q) = (rnd.next(), rnd.next());
            let v = if r % 3 == 0 {
                q
            } else if w % 4 == 0 {
                (g.pa(0) + (q % 512) as u64 * 0x1000) as u32
            } else if w % 4 == 2 {
                (q % 4) * 0x1000
            } else {
                0
            };
            g.extra(w, v);
        }
        let kind = types[(rnd.next() % types.len() as u32) as usize];
        g.cmd(0, kind);
        let (flags, ctx, ring) = (rnd.next() % 4, rnd.next() % 10, rnd.next() % 3);
        g.cmd(1, flags);
        g.cmd(4, ctx);
        g.cmd(5, ring);
        if kind == 0x200 && rnd.next() % 2 != 0 {
            g.cmd(6, 4);
            g.cmd(7, 6);
            g.cmd_bytes(8, b"test");
        }
        if kind == 0x10c {
            let mem = 1 + rnd.next() % 3;
            g.cmd(6, 30 + rnd.next() % 20);
            g.cmd(7, mem);
            g.cmd(9, if mem == 2 { 0 } else { 1 + rnd.next() % 3 });
            g.cmd(11, 0);
            g.cmd(12, (1 + rnd.next() % 3) * 0x1000);
            g.cmd(13, 0);
        }
        if kind == 0x102 || kind == 0x208 || kind == 0x202 {
            g.cmd(6, 30 + rnd.next() % 20);
        }
        if kind == 0x207 {
            g.cmd(6, rnd.next() % 80);
        }
        let words = 6 + rnd.next() % 30;
        let more = if rnd.next() % 3 != 0 { 0 } else { (rnd.next() % 64) * 4 };
        let room = if rnd.next() % 9 != 0 { 24 + rnd.next() % 200 } else { rnd.next() % 24 };
        g.ram.fill(0x31000, 0xee, 24);
        let mut n = g.g(words, more, room);
        let fenced = flags & 3 == 3 && ring != 0;
        if n == 0 && fenced {
            // held on a context's ring: retire it
            held += 1;
            fake.retire(ctx, ring, g.ram.u32(0x30008) as u64 | (g.ram.u32(0x3000c) as u64) << 32);
            n = g.returned();
        }
        assert_eq!(
            n, 1,
            "i {i} type {kind:#x} flags {flags} ctx {ctx} ring {ring} words {words} more {more} room {room}"
        );
        if room < 24 {
            continue; // no room for a response: returned unused
        }
        answered += 1;
        let r = g.resp(0);
        assert!((0x1100..=0x1106).contains(&r) || (0x1200..=0x1205).contains(&r));
        if r >= 0x1200 {
            errors += 1;
        }
        if flags & 1 != 0 {
            assert!(g.resp(1) & 1 != 0 && g.resp(2) == g.ram.u32(0x30008));
        }
        let live = st().live.iter().filter(|&&c| c != 0).count() as u32;
        assert!(live == st().contexts && live <= 8);
    }
    assert!(answered > 3000 && errors > 500 && errors < answered && held > 5, "{answered} {errors} {held}");
    // a reset forgets the contexts and the blobs, and the device starts again
    g.clear();
    for (i, v) in [(0, 0x200), (4, 3), (6, 4), (7, 6)] {
        g.cmd(i, v);
    }
    g.cmd_bytes(8, b"test");
    g.g(24, 0, 24);
    for (i, v) in [(0, 0x207), (1, 3), (2, 50), (5, 1), (6, 4)] {
        g.cmd(i, v);
    }
    g.g(9, 0, 24); // may be held across the reset
    g.d.write(0x70, 0);
    idle(&dev);
    assert!(st().contexts == 0 && st().blobs == 0);
    fake.retire(3, 1, 50); // late: its chain was the old queue's
    g.d = Driver::up(dev.clone() as Arc<dyn MmioDevice>, 0x59, 2, 8, g.pa(0x10000)).unwrap();
    g.clear();
    for (i, v) in [(0, 0x200), (4, 5), (6, 4), (7, 6)] {
        g.cmd(i, v);
    }
    g.cmd_bytes(8, b"test");
    assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1100 && st().contexts == 1);
    udmabufs_here(&g);
    // no drm2kgsl here (or no KGSL): refused, with a reason
    let why = gpu3d::kgsl_open(-1, g.pa(0), 1 << 20, std::ptr::null_mut(), g.pa(0), 8 << 20, |_, _| {}).err();
    assert!(why.is_some_and(|w| !w.is_empty()));
    // The two lanes: a blob an app's context is still making does not hold back the render server's submission
    // behind it.
    lanes(&mut g);
}

/// The guest driver's blob sizes (DroidVMVirtioGpu3D.cpp MaxBlobEntries, a 1 MiB request): Unity2025Poster's 80 MiB
/// blob of single pages (20480 entries) and 75022336 bytes (18316), refused on the phone on 2026-11-23 when the limit was
/// the display's 16384, are made, and so is the largest request the guest sends (65532 entries); one entry more is
/// refused, with its line. The 80 MiB blob's 20480 pages as udmabufs: three_d's udmabufs_here
/// (process-wide limits, so in that test's thread).
#[test]
fn blob_entries_of_the_guest_driver() {
    testing::install();
    let ram = GuestRam::new(4 << 20);
    let fake = Arc::new(Fake::default());
    let board = Arc::new(FakeBoard::default());
    board.st().frame_answer = true;
    board.display(8, 4, 32, 0);
    let dev = create_with(0, 43, 8, 4, Some(Box::new(fake.clone())), board.clone()).unwrap();
    dev.mmio_write(0x14, 4, 0);
    let d = Driver::up(dev.clone() as Arc<dyn MmioDevice>, 0x59, 2, 8, ram.pa(0x10000)).unwrap();
    let mut g = Guest { ram, d, board: board.clone(), dev: dev.clone() };
    let st = || fake.st();
    g.clear();
    for (i, v) in [(0, 0x200), (4, 5), (6, 4), (7, 6)] {
        g.cmd(i, v);
    }
    g.cmd_bytes(8, b"test");
    assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1100);
    // Entry i: one page of 256 every second page from 2 MiB (none adjacent to the next), the guest's ordinary pages.
    let base = g.pa(0);
    let page = move |i: usize| base + 0x200000 + (i as u64 % 256) * 0x2000;
    let segs = |n: usize| -> Vec<u64> { (0..n).flat_map(|i| [page(i), 0x1000]).collect() };
    for (res, n, size) in [(20u32, 20480u32, 83886080u32), (21, 18316, 75022336), (22, 65532, 65532 << 12)] {
        g.clear();
        for (i, v) in [(0, 0x10c), (4, 5), (6, res), (7, 3), (8, 9), (9, n), (10, res), (12, size)] {
            g.cmd(i, v);
        }
        g.extra_u64s(&segs(n as usize));
        assert!(g.g(14, n * 16, 24) == 1 && g.resp(0) == 0x1100, "{n} entries: {:#x}", g.resp(0));
        assert!(st().blob_size == size as u64 && st().entries.len() == n as usize && st().entries[1].addr == page(1));
    }
    // one more than the guest's request holds: refused before its entries are read
    g.clear();
    for (i, v) in [(0, 0x10c), (4, 5), (6, 23), (7, 3), (8, 9), (9, 65533), (10, 23), (12, 1 << 20)] {
        g.cmd(i, v);
    }
    let (ok, lines) = xh_core::log::capture(|| g.g(14, 16, 24) == 1 && g.resp(0) == 0x1205);
    assert!(ok && lines.iter().any(|(_, l)| l.contains("65533 entries") && l.contains("at most 65532")));
    assert_eq!(st().blobs, 3);
}

/// A udmabuf over guest segments: refused without touching the device node when a segment is not the RAM's; and real
/// ones where this host has /dev/udmabuf.
fn udmabufs_here(g: &Guest) {
    let (base, size) = (g.pa(0), 1u64 << 20);
    let outside = Entry::new(0x1000, 0x1000);
    let odd = Entry::new(base + 0x800, 0x1000);
    assert_eq!(gpu3d::udmabuf(-1, base, size, &[outside]).err(), Some(Errno(libc::EINVAL)));
    assert_eq!(gpu3d::udmabuf(-1, base, size, &[odd]).err(), Some(Errno(libc::EINVAL)));
    assert!(gpu3d::udmabuf(-1, base, size, &[]).is_err());
    // Three segments of a sealed memfd, two of them adjacent, read back through the dma-buf in the blob's order; a
    // memfd without the shrink seal is refused (mem.rs seals the RAM for this)
    if let Err(e) = std::fs::OpenOptions::new().read(true).write(true).open("/dev/udmabuf") {
        println!(
            "virtio-gpu 3D: no /dev/udmabuf here ({}): the real import is not checked",
            xh_core::sys::error_text(&e)
        );
        return;
    }
    // A memfd sealed against shrinking (what udmabuf takes), and one without the seal.
    let ram_fd = xh_core::fd::sealed_memfd(c"test-ram", 0, 1 << 20).unwrap();
    let bare = xh_core::fd::memfd(c"test-bare", 0).unwrap();
    std::fs::File::from(bare.try_clone().unwrap()).set_len(1 << 20).unwrap();
    let mut ram = oneios_display::sys::Map::new(ram_fd.as_fd(), 1 << 20, true).unwrap();
    for i in 0..256 {
        ram.bytes_mut()[i * 0x1000] = i as u8;
    }
    let rfd = ram_fd.as_raw_fd();
    let three =
        [Entry::new(base + 0x9000, 0x1000), Entry::new(base + 0x20000, 0x1000), Entry::new(base + 0x21000, 0x2000)];
    let buf = gpu3d::udmabuf(rfd, base, size, &three).unwrap();
    assert_eq!(seek_end(buf.as_raw_fd()), 0x4000);
    {
        let mut view = oneios_display::sys::Map::new(buf.as_fd(), 0x4000, true).unwrap();
        let v = view.bytes_mut();
        assert!(v[0] == 9 && v[0x1000] == 0x20 && v[0x2000] == 0x21 && v[0x3000] == 0x22);
        v[0x1001] = 0x5a; // the same pages, not a copy
        assert_eq!(ram.bytes()[0x20001], 0x5a);
    }
    drop(buf);
    assert_eq!(gpu3d::udmabuf(bare.as_raw_fd(), base, size, &three).err(), Some(Errno(libc::EINVAL)));
    // A blob past udmabuf's limits is several udmabufs in a row. With the limits set to 8 pages and 3 runs:
    gpu3d::udmabuf_set_limits(0x8000, 3);
    // 20 pages in one run: 8 + 8 + 4, by size
    let run = [Entry::new(base + 100 * 0x1000, 20 * 0x1000)];
    assert!(chunks_are(gpu3d::udmabufs(rfd, base, size, &run), &[8, 8, 4], &[100, 108, 116], 1));
    // 7 pages, every second one: 3 + 3 + 1, by runs
    let apart: Vec<Entry> = (0..7).map(|i| Entry::new(base + (10 + 2 * i) * 0x1000, 0x1000)).collect();
    assert!(chunks_are(gpu3d::udmabufs(rfd, base, size, &apart), &[3, 3, 1], &[10, 16, 22], 2));
    // two adjacent segments joined (4 pages, one run), then 6 pages: the second run is cut where the first udmabuf is full
    let mixed = [
        Entry::new(base + 30 * 0x1000, 0x2000),
        Entry::new(base + 32 * 0x1000, 0x2000),
        Entry::new(base + 40 * 0x1000, 0x6000),
    ];
    let chunks = gpu3d::udmabufs(rfd, base, size, &mixed).unwrap();
    assert!(chunks.len() == 2 && chunks[0].size == 0x8000 && chunks[1].size == 0x2000);
    {
        let a = oneios_display::sys::Map::new(chunks[0].fd.as_fd(), 0x8000, false).unwrap();
        let b = oneios_display::sys::Map::new(chunks[1].fd.as_fd(), 0x2000, false).unwrap();
        let (a, b) = (a.bytes(), b.bytes());
        assert!(a[0] == 30 && a[0x3000] == 33 && a[0x4000] == 40 && a[0x7000] == 43 && b[0] == 44 && b[0x1000] == 45);
    }
    drop(chunks);
    // exactly the limit is one udmabuf; one page more is not one
    let full = gpu3d::udmabuf(rfd, base, size, &[Entry::new(base + 50 * 0x1000, 0x8000)]).unwrap();
    assert_eq!(seek_end(full.as_raw_fd()), 0x8000);
    assert_eq!(
        gpu3d::udmabuf(rfd, base, size, &[Entry::new(base + 50 * 0x1000, 0x9000)]).err(),
        Some(Errno(libc::E2BIG))
    );
    // a bad segment anywhere in the list: nothing is made
    let bad = [Entry::new(base + 0x1000, 0x1000), Entry::new(base + (1 << 20), 0x1000)];
    assert_eq!(gpu3d::udmabufs(rfd, base, size, &bad).err(), Some(Errno(libc::EINVAL)));
    // The limits in use without a setting: the kernel's, never above 64 MiB and 1024 runs (the phone's). 72 MiB in one
    // run is 64 + 8 on any host; 1100 separate pages are 1024 + 76.
    gpu3d::udmabuf_set_limits(0, 0);
    let (bytes, runs) = gpu3d::udmabuf_limits();
    assert!(bytes != 0 && bytes <= 64 << 20 && runs != 0 && runs <= 1024);
    let big_fd = xh_core::fd::sealed_memfd(c"test-big", 0, 96 << 20).unwrap();
    let bfd = big_fd.as_raw_fd();
    if bytes == 64 << 20 && runs == 1024 {
        let big = [Entry::new(base + (8 << 20), 72 << 20)];
        let chunks = gpu3d::udmabufs(bfd, base, 96 << 20, &big).unwrap();
        assert!(chunks.len() == 2 && chunks[0].size == 64 << 20 && chunks[1].size == 8 << 20);
        {
            // the second begins where the first ends
            let mut a = oneios_display::sys::Map::new(chunks[0].fd.as_fd(), 64 << 20, true).unwrap();
            let mut b = oneios_display::sys::Map::new(chunks[1].fd.as_fd(), 8 << 20, true).unwrap();
            let all = oneios_display::sys::Map::new(big_fd.as_fd(), 96 << 20, false).unwrap();
            a.bytes_mut()[(64 << 20) - 1] = 0x11;
            b.bytes_mut()[0] = 0x22;
            b.bytes_mut()[(8 << 20) - 1] = 0x33;
            let all = all.bytes();
            assert!(all[(72 << 20) - 1] == 0x11 && all[72 << 20] == 0x22 && all[(80 << 20) - 1] == 0x33);
        }
        drop(chunks);
        let many: Vec<Entry> = (0..1100).map(|i| Entry::new(base + i * 0x2000, 0x1000)).collect();
        let chunks = gpu3d::udmabufs(bfd, base, 96 << 20, &many).unwrap();
        assert!(chunks.len() == 2 && chunks[0].size == 1024 * 0x1000 && chunks[1].size == 76 * 0x1000);
        drop(chunks);
        // Unity2025Poster's 80 MiB blob of single pages (20480 entries, refused on the phone on 2026-11-23 when the
        // device took 16384): 20 udmabufs of 1024 pages.
        let poster: Vec<Entry> = (0..20480).map(|i| Entry::new(base + (i % 8192) * 0x2000, 0x1000)).collect();
        let chunks = gpu3d::udmabufs(bfd, base, 96 << 20, &poster).unwrap();
        let whole = |c: &Chunk| c.size == 0x40_0000 && seek_end(c.fd.as_raw_fd()) == 0x40_0000;
        assert!(chunks.len() == 20 && chunks.iter().all(whole));
        drop(chunks);
        // A kernel that refuses a udmabuf over 16 MiB with ENOMEM (a 6.6 kernel's order-5 page array under
        // fragmentation, the S25 Ultra's `gpuctl large`): the limit halves, 64 -> 32 -> 16 MiB, and the same 72 MiB
        // blob is 16 + 16 + 16 + 16 + 8; the limit stays at 16 MiB for the next blob. Another error is returned as it is.
        gpu3d::set_udmabuf_create(Some(refuse_over_16));
        let chunks = gpu3d::udmabufs(bfd, base, 96 << 20, &big).unwrap();
        assert!(chunks.len() == 5 && chunks[0].size == 16 << 20 && chunks[4].size == 8 << 20);
        assert_eq!(seek_end(chunks[3].fd.as_raw_fd()), 16 << 20);
        drop(chunks);
        assert!(gpu3d::udmabuf_limits() == (16 << 20, 1024) && REFUSED.load(Ordering::Relaxed) == 2);
        REFUSE_ERRNO.store(libc::EINVAL, Ordering::Relaxed);
        gpu3d::udmabuf_set_limits(64 << 20, 1024);
        assert_eq!(gpu3d::udmabufs(bfd, base, 96 << 20, &big).err(), Some(Errno(libc::EINVAL)));
        assert_eq!(gpu3d::udmabuf_limits().0, 64 << 20); // not ENOMEM: kept
        gpu3d::set_udmabuf_create(None);
        gpu3d::udmabuf_set_limits(0, 0);
        println!("virtio-gpu 3D: blobs past udmabuf's limits (64 MiB, 1024 runs) split and checked on this host");
    } else {
        println!(
            "virtio-gpu 3D: this host's udmabuf limits are {} MiB and {runs} runs: the 64 MiB split is not checked",
            bytes >> 20
        );
    }
    println!("virtio-gpu 3D: udmabuf checked on this host");
}

fn lanes(g: &mut Guest) {
    let dev = g.dev.clone();
    g.clear();
    for (i, v) in [(0, 0x200), (4, 40), (6, 4), (7, 6)] {
        g.cmd(i, v);
    }
    g.cmd_bytes(8, b"test");
    assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1100);
    g.clear();
    for (i, v) in [(0, 0x200), (4, 41), (6, 14), (7, 6)] {
        g.cmd(i, v);
    }
    g.cmd_bytes(8, b"xnu:backboardd");
    assert!(g.g(24, 0, 24) == 1 && g.resp(0) == 0x1100);
    let (ca, ra, cb, rb, cc, rc) = (0x33000, 0x34000, 0x35000, 0x36000, 0x37000, 0x38000);
    for c in [ca, cb, cc] {
        g.ram.fill(c, 0, 4096);
    }
    for r in [ra, rb, rc] {
        g.ram.fill(r, 0xee, 64);
    }
    // the app's blobs, held
    for (i, v) in [(0, 0x10c), (4, 40), (6, 600), (7, 2), (10, 0x5105), (12, 16384)] {
        g.ram.set_u32(ca + 4 * i, v);
    }
    for (i, v) in [(0, 0x10c), (4, 40), (6, 601), (7, 2), (10, 0x5106), (12, 16384)] {
        g.ram.set_u32(cc + 4 * i, v);
    }
    // the render server's SUBMIT_3D
    for (i, v) in [(0, 0x207), (4, 41), (6, 8)] {
        g.ram.set_u32(cb + 4 * i, v);
    }
    g.ram.write(cb + 32, b"frame!!!");
    BLOB_HELD.store(3, Ordering::Release);
    let pa = |o: usize| g.ram.pa(o);
    let (pca, pra, pcc, prc, pcb, prb) = (pa(ca), pa(ra), pa(cc), pa(rc), pa(cb), pa(rb));
    // Post all three chains and kick once: a kick of its own would reap a response here (send_at's reap), and the
    // count below must see all three.
    g.d.send(0, &[(pca, 14 * 4, false), (pra, 24, true)], false);
    g.d.send(0, &[(pcc, 14 * 4, false), (prc, 24, true)], false);
    g.d.send_at(0, 2, &[(pcb, 10 * 4, false), (prb, 24, true)], false); // three chains at once: 0, 4, 2
    g.d.kick(0);
    // The submit is answered while both blobs wait; or, when the slow lane took the first blob's turn before the
    // submit came, right after that one blob and before the second (the fast lane's turn at the lock), however the
    // threads run.
    let mut released = false;
    for _ in 0..20000 {
        if g.ram.u32(rb) == 0x1100 {
            break;
        }
        if !released && BLOB_IN.load(Ordering::Acquire) == 0x5105 && fast_waiting(&dev) != 0 {
            BLOB_HELD.store(2, Ordering::Release);
            released = true;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(g.ram.u32(rb) == 0x1100 && g.ram.u32(rc) == 0xeeee_eeee && (released || g.ram.u32(ra) == 0xeeee_eeee));
    BLOB_HELD.store(0, Ordering::Release);
    idle(&dev);
    assert_eq!(g.ram.u32(ra), 0x1100, "the first blob's chain");
    assert_eq!(g.ram.u32(rc), 0x1100, "the second blob's chain");
    assert_eq!(g.returned(), 3, "the chains returned");
    g.clear();
    g.cmd(0, 0x201);
    g.cmd(4, 40);
    assert!(g.g(6, 0, 24) == 1 && g.resp(0) == 0x1100);
    g.cmd(4, 41);
    assert!(g.g(6, 0, 24) == 1 && g.resp(0) == 0x1100);
}
