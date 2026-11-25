// SPDX-License-Identifier: MIT
//! virtio-gpu 3D on KGSL: Droid-VM's drm2kgsl (external/virglrenderer src/drm/drm2kgsl, MIT), linked into xnuhost
//! through drm2kgsl-sys. drm2kgsl is virglrenderer's DRM native context for the msm protocol, served on /dev/kgsl-3d0.
//! This is what virglrenderer.c and crosvm's rutabaga are around it in a DroidVM, cut down to the one renderer: the
//! context and resource tables, a blob made from a context object, the guest's pages handed over as udmabufs, and the
//! fences. virglrenderer.c itself is not built: it pulls in the GL renderer (epoxy, EGL) and libdrm, and Android has
//! neither.
//!
//! The pool (/arm-io/virtio-gpu drm-host-pool) is drm2kgsl's "arena": it reads the memfd, the offset in it, the host
//! address and the size from CROSVM_DRM2KGSL_ARENA_*, set here before the first context. A context's msm shmem is
//! carved from it; every BO is the guest's own pages (MSM_BO_GUEST_ALLOC).
pub mod policy;

use std::collections::HashMap;
use std::os::fd::{AsFd, RawFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use xh_core::bytes::le;

use drm2kgsl_sys as d2k;
use policy::{Action, Class, Frame, Mode, Pages, Policy};

use crate::gpu3d::{self, Chunk, Entry, Errno, Gpu3d, LogFn, Retire};

// ---- the log ---------------------------------------------------------------------------------------------------------
static SINK: AtomicUsize = AtomicUsize::new(0);
/// gpu-down=bottom (docs/switches.md), set once by session.rs before the GPU opens.
pub static DOWN_AT_BOTTOM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// gpu-kgsl-ui-rb=1 (docs/switches.md), set once by session.rs before the GPU opens.
pub static UI_RB: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// gpu-off-level=1 (docs/switches.md), set once by session.rs before the GPU opens.
pub static OFF_LEVEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// A line to the sink set at the open: at most 511 bytes, line breaks at its end dropped (as say() did).
fn say(level: u8, line: &str) {
    let p = SINK.load(Ordering::Acquire);
    if p == 0 {
        return;
    }
    let line = crate::input::clip(line, 511);
    // SAFETY: SINK only ever holds 0 or a `LogFn` stored by open.
    let f: LogFn = unsafe { std::mem::transmute::<usize, LogFn>(p) };
    f(level, line.trim_end_matches('\n'));
}

macro_rules! say {
    ($level:expr, $($arg:tt)*) => { say($level, &format!($($arg)*)) };
}

fn renderer_line(level: u8, line: &str) {
    say(level, line)
}

// ---- the host's share of a frame --------------------------------------------------------------------------------------
// The guest's GPU report times a render-server frame from its submission to the fence it sees signalled (9-11 ms on
// the phone with KGSL 7 % busy, 2026-11-12). This times the host's part of the same path, per 10 s, one log line:
//   gpu: 10 s host: backboardd fences 98, submit->retired mean 2100 us max 9000 us; submit calls mean 150 us max 900 us;
//        other fences 30, mean 1800 us
// "submit->retired" is from the fence's command reaching the renderer to drm2kgsl's retire callback (the GPU's work and
// KGSL's wake-up); "submit calls" is the time in drm2kgsl's submit (the KGSL ioctl and the command's checks). The rest
// of the guest's figure is the virtio round trip and the guest's own wake-ups.
const STAT_RINGS: usize = 4;
const STAT_PENDING: usize = 64;
const STAT_CTX: usize = 128;

/// A fence on its way. `bg`: the context was background when it submitted (fg_pending counts the others).
#[derive(Clone, Copy, Default)]
struct Pending {
    fence: u64,
    at: u64,
    frame: Frame,
    bg: bool,
}

/// One context's figures and its class. The GPU's policy (policy.rs): the context's name, the guest's last hint, its
/// class from them, the frame its last GEM_SUBMIT started (for the fence that follows), and each ring's last
/// retirement. `shown`: the render server reads one of its surfaces; `shared`: another process does (the render
/// server or not); Pages, create_blob. `slow`, `slow_max`: its fences over FENCE_SLOW_US since the last 10 s line.
#[derive(Clone, Copy)]
struct StatCtx {
    id: u32,
    render: bool,
    head: [usize; STAT_RINGS],
    count: [usize; STAT_RINGS],
    pending: [[Pending; STAT_PENDING]; STAT_RINGS],
    name: [u8; 40], // "xnu:" and p_name (at most 32)
    name_len: usize,
    hint: [u8; 3],
    cls: Class,
    said: bool,
    lowest: bool,
    has_next: bool,
    shown: bool,
    shared: bool,
    slow: u64,
    slow_max: u64,
    next: Frame,
    last_retire: [u64; STAT_RINGS],
}

impl StatCtx {
    const EMPTY: StatCtx = StatCtx {
        id: 0,
        render: false,
        head: [0; STAT_RINGS],
        count: [0; STAT_RINGS],
        pending: [[Pending {
            fence: 0,
            at: 0,
            frame: Frame { level: policy::Level::Governor, cold: false, budget_us: 0 },
            bg: false,
        }; STAT_PENDING]; STAT_RINGS],
        name: [0; 40],
        name_len: 0,
        hint: [0; 3],
        cls: Class::Default,
        said: false,
        lowest: false,
        has_next: false,
        shown: false,
        shared: false,
        slow: 0,
        slow_max: 0,
        next: Frame { level: policy::Level::Governor, cold: false, budget_us: 0 },
        last_retire: [0; STAT_RINGS],
    };

    fn name(&self) -> &[u8] {
        &self.name[..self.name_len]
    }

    fn oldest(&self, r: usize) -> Option<&Pending> {
        (self.count[r] > 0).then(|| &self.pending[r][self.head[r]])
    }
}

#[derive(Default)]
struct Totals {
    render_n: u64,
    render_sum: u64,
    render_max: u64,
    other_n: u64,
    other_sum: u64,
    submit_n: u64,
    submit_sum: u64,
    submit_max: u64,
    blob_n: u64,
    blob_sum: u64,
    blob_max: u64,
    udma_sum: u64,
}

/// The 10 s lines' last figures of the level.
#[derive(Default)]
struct Said {
    frames: u64,
    top: u64,
    ups: u64,
    downs: u64,
    misses: u64,
    late: u64,
    lowest: u64,
    refused: u64,
    undone: u64,
}

struct St {
    ctx: Box<[StatCtx; STAT_CTX]>,
    since: u64,
    t: Totals,
    holds: u64,
    keepalives: u64,
    /// the import line's last figures (0007)
    dropped: u64,
    plain: u64,
    unreadable: u64,
    imports_said: bool,
    /// The level (policy.rs) and the 10 s line's last figures of it.
    pol: Policy,
    /// the name lists (None: the policy's defaults)
    background: Option<String>,
    front: Option<String>,
    lowest_n: u64,
    said: Said,
    /// gpu-priority=1: background queues can carry KGSL's lowest level (patch 0008)
    priority_on: bool,
    /// fences of non-background contexts not yet retired (lowest_for)
    fg_pending: u32,
    /// whose surfaces another process reads (Pages::note)
    pages: Pages,
    /// the device's answer to a retired fence
    retire: Option<Retire>,
    /// "gpu: context" lines said (at most 400, then every 100th)
    context_lines: u32,
}

static ST: Mutex<Option<St>> = Mutex::new(None);

fn st() -> MutexGuard<'static, Option<St>> {
    ST.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_us() -> u64 {
    xh_core::time::monotonic_ns() / 1000
}

impl St {
    fn find(&mut self, id: u32) -> Option<&mut StatCtx> {
        self.ctx.iter_mut().find(|c| c.id == id)
    }

    fn class_of(&self, name: &[u8], hint: u8, qos: u8, shown: bool) -> Class {
        policy::class_of(name, hint, qos, shown, self.background.as_deref(), self.front.as_deref())
    }

    fn report(&mut self, now: u64) {
        if self.since == 0 {
            self.since = now;
            return;
        }
        if now - self.since < 10_000_000 {
            return;
        }
        let secs = (now - self.since) / 1_000_000;
        let (holds, keepalives) = d2k::hold_stats();
        let t = &self.t;
        let mean = |sum: u64, n: u64| sum.checked_div(n).unwrap_or(0);
        if t.render_n != 0 || t.other_n != 0 || t.blob_n != 0 {
            say!(
                1,
                "gpu: {} s host: backboardd fences {}, submit->retired mean {} us max {} us; submit calls mean {} us max {} us; \
                 other fences {}, mean {} us; blobs made {}, mean {} us max {} us (udmabufs {} us of it); clock held +{} times, \
                 keep-alives +{}",
                secs,
                t.render_n,
                mean(t.render_sum, t.render_n),
                t.render_max,
                mean(t.submit_sum, t.submit_n),
                t.submit_max,
                t.other_n,
                mean(t.other_sum, t.other_n),
                t.blob_n,
                mean(t.blob_sum, t.blob_n),
                t.blob_max,
                mean(t.udma_sum, t.blob_n),
                holds.wrapping_sub(self.holds),
                keepalives.wrapping_sub(self.keepalives)
            );
        }
        self.holds = holds;
        self.keepalives = keepalives;
        // The level (policy.rs, docs/gpu.md 2.2b): how many counted frames ran at the top, how often it went up and
        // down, frames that (nearly) missed at the governor's clock, frames over their budget at any level.
        let (p, s) = (&self.pol, &mut self.said);
        if p.frames != s.frames
            || self.lowest_n != s.lowest
            || p.lowest_refused != s.refused
            || p.lowest_undone != s.undone
        {
            say!(
                1,
                "gpu: {} s level: frames {}, at the top {}; up +{} down +{}; missed at the governor's +{}, over budget +{}; \
                 GPU time at the top {} us, the governor's {} us (x{}.{:02} of the top's{}), after a pause {} us; frame interval {} us; background \
                 submissions at the lowest +{} (kept off it: others' work queued, surfaces read, or waited {} ms +{}; ended by \
                 others' work +{})",
                secs,
                p.frames - s.frames,
                p.frames_top - s.top,
                p.ups - s.ups,
                p.downs - s.downs,
                p.misses - s.misses,
                p.late - s.late,
                p.top_us,
                p.gov_us,
                p.at_governor(16) / 16,
                p.at_governor(16) % 16 * 100 / 16,
                if p.gov_x16 != 0 { ", measured" } else { ", guessed" },
                p.cold_us,
                p.interval_us,
                self.lowest_n - s.lowest,
                p.bg_bound_us / 1000,
                p.lowest_refused - s.refused,
                p.lowest_undone - s.undone
            );
        }
        *s = Said {
            frames: p.frames,
            top: p.frames_top,
            ups: p.ups,
            downs: p.downs,
            misses: p.misses,
            late: p.late,
            lowest: self.lowest_n,
            refused: p.lowest_refused,
            undone: p.lowest_undone,
        };
        // Which processes' fences took longer than one of Metal's 100 ms wait slices (each is one of the guest's
        // "fence timeouts" in oneios-gpu-report), and which still wait that long now: one line, only when there are
        // any (docs/gpu.md 2.2b).
        let mut line = String::new();
        let (mut shown, mut more) = (0, 0);
        for c in self.ctx.iter_mut().filter(|c| c.id != 0) {
            let mut waiting = (0..STAT_RINGS).filter_map(|r| c.oldest(r)).map(|p| now - p.at).max().unwrap_or(0);
            if waiting < policy::FENCE_SLOW_US {
                waiting = 0;
            }
            if c.slow == 0 && waiting == 0 {
                continue;
            }
            if shown == 6 || line.len() >= 640 - 160 {
                // an item is at most ~150
                more += 1;
                c.slow = 0;
                c.slow_max = 0;
                continue;
            }
            let now_waiting =
                if waiting != 0 { format!(", one waiting now {} ms", waiting / 1000) } else { String::new() };
            line += &format!(
                "{}{} {} (max {} ms{}; {})",
                if shown != 0 { ", " } else { "" },
                String::from_utf8_lossy(c.name()),
                c.slow,
                c.slow_max / 1000,
                now_waiting,
                c.cls.name()
            );
            shown += 1;
            c.slow = 0;
            c.slow_max = 0;
        }
        if shown != 0 {
            say!(
                1,
                "gpu: {} s fences over {} ms (each one of the guest's fence timeouts): {}{}",
                secs,
                policy::FENCE_SLOW_US / 1000,
                line,
                if more != 0 { " and more" } else { "" }
            );
        }
        // KGSL's coherence of the guest's imports (drm2kgsl patch 0007): a line the first time, then only when an
        // import lost its IO coherence, was imported without it, or could not be read back (docs/gpu.md 2.1, "0007").
        let i = d2k::import_stats();
        if i.checked != 0
            && (!self.imports_said
                || i.dropped != self.dropped
                || i.plain != self.plain
                || i.unreadable != self.unreadable)
        {
            say!(
                1,
                "gpu: KGSL imports {}, read back {}: IO-coherent {}, coherence dropped by KGSL {}, not coherent as asked {}, \
                 not readable {}",
                i.imports,
                i.checked,
                i.kept,
                i.dropped,
                i.plain,
                i.unreadable
            );
            self.imports_said = true;
            (self.dropped, self.plain, self.unreadable) = (i.dropped, i.plain, i.unreadable);
        }
        self.since = now;
        self.t = Totals::default();
    }
}

/// drm2kgsl's retire callback: the stats, the level's frame, then the device's answer.
fn fence_retired(ctx: u32, ring: u32, fence: u64) {
    let now = now_us();
    let retire = {
        let mut g = st();
        let Some(s) = g.as_mut() else { return };
        let mut done: Option<Pending> = None;
        let mut fg_done = 0;
        let mut t = std::mem::take(&mut s.t);
        if let Some(c) = s.find(ctx).filter(|_| (ring as usize) < STAT_RINGS) {
            let r = ring as usize;
            // Mergeable fences: this one and every earlier one of the ring are done.
            while c.count[r] > 0 {
                let p = c.pending[r][c.head[r]];
                if p.fence > fence {
                    break;
                }
                let took = now - p.at;
                if c.render {
                    crate::frame_path::render_retired(now, took);
                    t.render_n += 1;
                    t.render_sum += took;
                    t.render_max = t.render_max.max(took);
                } else {
                    t.other_n += 1;
                    t.other_sum += took;
                }
                if took >= policy::FENCE_SLOW_US {
                    c.slow += 1;
                    c.slow_max = c.slow_max.max(took);
                }
                if !p.bg {
                    fg_done += 1;
                }
                done = Some(p);
                c.head[r] = (c.head[r] + 1) % STAT_PENDING;
                c.count[r] -= 1;
            }
            // The frame's GPU time, for the level: from its submission, or from the ring's previous retirement when the
            // GPU was still on the frame before it, to this one (KGSL's timestamp passed). Fences retired together count
            // once, for the newest.
            let from_last = c.last_retire[r];
            c.last_retire[r] = now;
            if let Some(newest) = done.filter(|p| p.at != 0) {
                let from = newest.at.max(from_last);
                let gpu = now.saturating_sub(from);
                s.pol.frame_done(&newest.frame, gpu.min(u32::MAX as u64) as u32);
            }
        }
        s.t = t;
        s.fg_pending = s.fg_pending.saturating_sub(fg_done);
        s.report(now);
        s.retire.clone()
    };
    if let Some(f) = retire {
        f(ctx, ring, fence);
    }
}

// ---- the renderer -----------------------------------------------------------------------------------------------------
/// Whether a SUBMIT_3D's ccmds include a GEM_SUBMIT (MSM_CCMD_GEM_SUBMIT, 7): work for the GPU, a frame for the level.
fn has_gem_submit(commands: &[u8]) -> bool {
    let size = commands.len();
    let mut at = 0;
    while at + 16 <= size {
        // vdrm_ccmd_req: cmd, len, seqno, rsp_off
        let cmd = le::u32(commands, at);
        let len = le::u32(commands, at + 4) as usize;
        if cmd == 7 {
            return true;
        }
        if len < 16 || len > size - at {
            return false;
        }
        at += len;
    }
    false
}

/// The renderer: drm2kgsl and what xnuhost keeps around it.
struct Kgsl {
    capset: d2k::Capset,
    ram_fd: RawFd,
    ram_phys: u64,
    ram_size: u64,
    pool_host: usize,
    pool_size: u64,
    /// Blobs of more than one udmabuf. One udmabuf holds at most 64 MiB and 1024 runs of pages (gpu3d.rs), so a larger
    /// or more scattered blob is several, "chunks". virglrenderer's resource has room for one fd; the chunks of such a
    /// resource are kept here, by resource id, for a second context that attaches it (drm2kgsl imports every chunk
    /// again). Only the device's lanes come here.
    chunked: Mutex<HashMap<u32, Vec<Chunk>>>,
}

impl Kgsl {
    fn chunked(&self) -> MutexGuard<'_, HashMap<u32, Vec<Chunk>>> {
        self.chunked.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[allow(clippy::too_many_arguments)]
    fn create_blob_now(
        &self,
        id: u32,
        res: u32,
        blob_mem: u32,
        flags: u32,
        blob_id: u64,
        size: u64,
        entries: &[Entry],
    ) -> Result<(), Errno> {
        if res == 0 || d2k::resource_exists(res) {
            return Err(Errno(libc::EINVAL));
        }
        // The guest's pages; with more than one chunk the list stays ours (chunked).
        let mut chunks = Vec::new();
        if !entries.is_empty() {
            let u0 = now_us();
            let made = gpu3d::udmabufs(self.ram_fd, self.ram_phys, self.ram_size, entries);
            let u1 = now_us();
            if let Some(s) = st().as_mut() {
                s.t.udma_sum += u1 - u0;
            }
            chunks = match made {
                Ok(c) => c,
                Err(e) => {
                    say!(0, "gpu: udmabuf over {} segments of a {} byte blob: {}", entries.len(), size, e);
                    return Err(e);
                }
            };
            if chunks.len() > 1 {
                say!(
                    2,
                    "gpu: resource {}: {} bytes in {} segments as {} udmabufs",
                    res,
                    size,
                    entries.len(),
                    chunks.len()
                );
            }
        }
        let dup = |c: &Chunk| c.fd.try_clone().map_err(|_| Errno(libc::EMFILE));
        if blob_mem == gpu3d::BLOB_GUEST {
            // The display's surface: known as a dma-buf (its first, when it is chunked), for a context that attaches it.
            let Some(first) = chunks.first() else { return Err(Errno(libc::ENOMEM)) };
            d2k::resource_dmabuf(res, dup(first)?, size).map_err(|_| Errno(libc::ENOMEM))?;
            if chunks.len() > 1 {
                self.chunked().insert(res, chunks);
            }
            return Ok(());
        }
        if !d2k::context_exists(id) {
            return Err(Errno(libc::EINVAL));
        }
        // The pages of a guest-allocated BO are parked in the context for the get_blob that follows. It takes the fds
        // it is given: copies of them, so that the chunk list above stays whole.
        if !chunks.is_empty() {
            let copies: Result<Vec<_>, Errno> = chunks.iter().map(|c| Ok((dup(c)?, c.size))).collect();
            let copies = copies.map_err(|_| Errno(libc::EINVAL))?;
            d2k::set_guest_blob_chunks(id, blob_id, copies).map_err(|_| Errno(libc::EINVAL))?;
        }
        d2k::get_blob(id, res, blob_id, size, flags & !gpu3d::BLOB_FLAG_GUEST_HANDLE)?;
        if chunks.len() > 1 {
            self.chunked().insert(res, chunks);
        }
        Ok(())
    }

    /// Whose surfaces another process reads (policy.rs Pages): context `id` made resource `res` over guest pages that
    /// start at `page`. When another live context made a blob over the same first page, both are `shared`; when one of
    /// the two is the render server, the other is `shown`, which keeps it out of the background class for the rest of
    /// its life (a widget renderer, a poster, a keyboard: the user sees what it draws). Two contexts of one process
    /// (every Metal process holds two) are not another process. On the device's lanes, as virglrenderer's tables are.
    fn note_pages(&self, id: u32, res: u32, page: u64) {
        let seen = {
            let mut g = st();
            let Some(s) = g.as_mut() else { return };
            let Some(other) = s.pages.note(page, res, id) else { return };
            let (Some(ai), Some(oi)) =
                (s.ctx.iter().position(|c| c.id == id), s.ctx.iter().position(|c| c.id == other.ctx))
            else {
                return;
            };
            if ai == oi || !d2k::resource_exists(other.res) || s.ctx[ai].name() == s.ctx[oi].name() {
                return;
            }
            s.ctx[ai].shared = true;
            s.ctx[oi].shared = true;
            if s.ctx[ai].render == s.ctx[oi].render {
                return;
            }
            let si = if s.ctx[ai].render { oi } else { ai };
            if s.ctx[si].shown {
                return;
            }
            let c = s.ctx[si];
            let was = c.cls;
            let cls = s.class_of(c.name(), c.hint[0], c.hint[1], true);
            let seen = &mut s.ctx[si];
            seen.shown = true;
            seen.cls = cls;
            if cls != was {
                seen.lowest = false;
            }
            (seen.id, seen.name, seen.name_len, was, cls)
        };
        let (seen_id, name, name_len, was, cls) = seen;
        if cls != was {
            d2k::set_class(seen_id, cls as u32, false);
        }
        say!(
            1,
            "gpu: context {} {}: the render server reads its surfaces{}{}{}",
            seen_id,
            String::from_utf8_lossy(&name[..name_len]),
            if cls != was { ": " } else { " (" },
            cls.name(),
            if cls != was { " now, not background" } else { ")" }
        );
    }
}

impl Gpu3d for Kgsl {
    fn capset_count(&self) -> u32 {
        1
    }

    fn capset_info(&self, index: u32) -> Option<(u32, u32, u32)> {
        (index == 0).then(|| (gpu3d::CAPSET_DRM, 0, self.capset.bytes().len() as u32))
    }

    fn capset(&self, id: u32, _version: u32, out: &mut [u8]) -> usize {
        let b = self.capset.bytes();
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
        if d2k::context_exists(id) {
            return Err(Errno(libc::EEXIST));
        }
        let fd = d2k::sys::open(c"/dev/kgsl-3d0", libc::O_RDWR | libc::O_CLOEXEC)
            .map_err(|e| Errno(e.raw_os_error().unwrap_or(libc::EIO)))?;
        d2k::context_create(id, fd, name).map_err(|_| Errno(libc::ENOMEM))?;
        let cls = {
            let mut g = st();
            let Some(s) = g.as_mut() else { return Ok(()) };
            let cls = s.class_of(name, 0, 0, false);
            if let Some(c) = s.find(0) {
                *c = StatCtx::EMPTY;
                c.id = id;
                c.render = name == b"xnu:backboardd";
                c.name_len = name.len().min(c.name.len() - 1);
                c.name[..c.name_len].copy_from_slice(&name[..c.name_len]);
                c.cls = cls;
            }
            cls
        };
        d2k::set_class(id, cls as u32, false); // by name until the guest's hint (next) says more
        Ok(())
    }

    /// The guest's hint (gpu3d.rs): right after CTX_CREATE, then before each SUBMIT_3D. The class follows the
    /// submitting task's role (an app sent to the background stops counting for the level at once); the KGSL priority
    /// is the class's when a queue is made.
    fn hint(&self, id: u32, h: [u8; 3]) {
        if !d2k::context_exists(id) {
            return;
        }
        let (first, was, cls, lowest, shown, name, name_len, number, background, front) = {
            let mut g = st();
            let Some(s) = g.as_mut() else { return };
            let Some(i) = s.ctx.iter().position(|c| c.id == id) else { return };
            let c = s.ctx[i];
            // Unfenced submissions (and an older guest) carry no hint: the last one the context had stays.
            if c.said && h[0] & policy::HINT_VALID == 0 {
                return;
            }
            let first = !c.said;
            let changed = first || c.hint[0] != h[0] || c.hint[1] != h[1];
            let new_cls = if changed { s.class_of(c.name(), h[0], h[1], c.shown) } else { c.cls };
            let ctx = &mut s.ctx[i];
            ctx.hint[2] = h[2];
            if changed {
                ctx.hint[0] = h[0];
                ctx.hint[1] = h[1];
                ctx.cls = new_cls;
            }
            ctx.said = true;
            if !changed {
                return;
            }
            let said = first || new_cls != c.cls;
            if said {
                s.context_lines += 1;
            }
            (
                first,
                c.cls,
                new_cls,
                ctx.lowest,
                ctx.shown,
                ctx.name,
                ctx.name_len,
                said.then_some(s.context_lines),
                s.background.clone(),
                s.front.clone(),
            )
        };
        if first || cls != was {
            d2k::set_class(id, cls as u32, lowest);
        }
        // One line per context (they are made once a process: two each), and when its class changes.
        if number.is_some_and(|n| n <= 400 || n % 100 == 0) {
            let name = &name[..name_len];
            let class_of =
                |hint: u8, qos: u8| policy::class_of(name, hint, qos, false, background.as_deref(), front.as_deref());
            let why = if h[0] & policy::HINT_VALID == 0 {
                "by name: the guest sent no role"
            } else if cls == Class::Render {
                "the render server, by name"
            } else if shown && class_of(h[0], h[1]) == Class::Background {
                "not background: the render server reads its surfaces"
            } else if class_of(0, 0) != Class::Default {
                "by name"
            } else {
                "by the guest's role"
            };
            say!(
                1,
                "gpu: context {} {}: {} ({}; role {}{}, QoS {}), KGSL priority {}",
                id,
                String::from_utf8_lossy(name),
                cls.name(),
                why,
                h[0] & policy::HINT_ROLE,
                if h[0] & policy::HINT_DARWINBG != 0 { ", DarwinBG" } else { "" },
                h[1] & 7,
                policy::kgsl_priority_for(cls, UI_RB.load(Ordering::Relaxed))
            );
        }
    }

    fn ctx_destroy(&self, id: u32) {
        d2k::context_destroy(id);
        let mut g = st();
        let Some(s) = g.as_mut() else { return };
        if id == 0 {
            return;
        }
        let Some(i) = s.ctx.iter().position(|c| c.id == id) else { return };
        let c = &s.ctx[i];
        // its fences that will never retire now
        let fg = (0..STAT_RINGS)
            .flat_map(|r| (0..c.count[r]).map(move |k| (r, k)))
            .filter(|&(r, k)| !c.pending[r][(c.head[r] + k) % STAT_PENDING].bg)
            .count() as u32;
        s.fg_pending = s.fg_pending.saturating_sub(fg);
        s.ctx[i] = StatCtx::EMPTY;
    }

    fn submit(&self, id: u32, commands: &[u8]) -> Result<(), Errno> {
        if !d2k::context_exists(id) || commands.len() & 3 != 0 {
            return Err(Errno(libc::EINVAL));
        }
        // The level for this work, before it reaches KGSL (policy.rs): a frame of the render server, or of a foreground
        // context within a second of one, gets the top level only if it would miss its deadline at the governor's clock.
        if has_gem_submit(commands) {
            let (action, lowest, lowest_was, cls) = {
                let mut g = st();
                let Some(s) = g.as_mut() else { return Err(Errno(libc::EINVAL)) };
                match s.ctx.iter().position(|c| c.id == id) {
                    None => (Action::None, false, false, Class::Default),
                    Some(i) => {
                        let now = now_us() * 1000;
                        let c = s.ctx[i];
                        let display_off = OFF_LEVEL.load(Ordering::Relaxed) && !onebridges::brightness::display_on();
                        let (action, next, lowest) = if display_off {
                            let action = if s.pol.level == policy::Level::Top {
                                s.pol.level = policy::Level::Governor;
                                Action::Governor
                            } else {
                                Action::None
                            };
                            (action, Frame::default(), s.priority_on)
                        } else {
                            let (action, next) =
                                s.pol.frame_start(c.cls, now, c.hint[2] as u32 * policy::HINT_DEADLINE_US);
                            // KGSL's lowest level for a background context (gpu-priority=1) at a still screen, unless other
                            // work is queued, another process reads its surfaces, or its own oldest fence has waited past
                            // the bound (lowest_for).
                            let oldest = (0..STAT_RINGS).filter_map(|r| c.oldest(r)).map(|p| p.at).min().unwrap_or(0);
                            let lowest = c.cls == Class::Background
                                && s.priority_on
                                && s.pol.lowest_for(now, s.fg_pending, c.shared, oldest * 1000);
                            (action, next, lowest)
                        };
                        if lowest {
                            s.lowest_n += 1;
                        }
                        let ctx = &mut s.ctx[i];
                        ctx.next = next;
                        ctx.has_next = next.budget_us != 0;
                        ctx.lowest = lowest;
                        (action, lowest, c.lowest, c.cls)
                    }
                }
            };
            if action != Action::None {
                d2k::set_level(action == Action::Top);
            }
            if lowest != lowest_was {
                d2k::set_class(id, cls as u32, lowest);
            }
        }
        // drm_context reads the stream as 32-bit words in place: the device's copy is word-aligned.
        let start = now_us();
        let r = match bytemuck_words(commands) {
            Some(words) => d2k::submit(id, words),
            None => Err(Errno(libc::EINVAL)),
        };
        let took = now_us() - start;
        if let Some(s) = st().as_mut() {
            s.t.submit_n += 1;
            s.t.submit_sum += took;
            s.t.submit_max = s.t.submit_max.max(took);
        }
        r.map_err(|_| Errno(libc::EINVAL))
    }

    fn submit_fence(&self, id: u32, ring: u32, fence: u64) -> Result<(), Errno> {
        if !d2k::context_exists(id) {
            return Err(Errno(libc::EINVAL));
        }
        if let Some(s) = st().as_mut() {
            let mut fg = 0;
            if let Some(c) = s.find(id) {
                let r = ring as usize;
                if r < STAT_RINGS && c.count[r] < STAT_PENDING {
                    let at = (c.head[r] + c.count[r]) % STAT_PENDING;
                    let frame = if c.has_next { c.next } else { Frame::default() };
                    c.pending[r][at] = Pending { fence, at: now_us(), frame, bg: c.cls == Class::Background };
                    c.count[r] += 1;
                    if c.render {
                        crate::frame_path::render_submit(now_us());
                    }
                    if c.cls != Class::Background {
                        fg = 1;
                    }
                }
                c.has_next = false;
            }
            s.fg_pending += fg;
        }
        d2k::submit_fence(id, ring, fence)
    }

    /// The cost of a blob on the host (udmabufs of its pages, drm2kgsl's import and bind): one per guest BO, and a
    /// process drawing icons made 1335 under 1 MiB (phone, 2026-11-12). Counted in the 10 s host line.
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
        let r = self.create_blob_now(id, res, blob_mem, flags, blob_id, size, entries);
        let took = now_us() - start;
        if let Some(s) = st().as_mut() {
            s.t.blob_n += 1;
            s.t.blob_sum += took;
            s.t.blob_max = s.t.blob_max.max(took);
            s.report(start + took);
        }
        if r.is_ok() && id != 0 && blob_mem != gpu3d::BLOB_GUEST {
            if let Some(first) = entries.first() {
                self.note_pages(id, res, first.addr);
            }
        }
        r
    }

    fn map_blob(&self, res: u32) -> Result<(u64, u32), Errno> {
        let (ptr, size, info) = d2k::resource_map(res).ok_or(Errno(libc::EINVAL))?;
        let at = ptr.map_or(0, |p| p.as_ptr() as usize);
        let (lo, hi) = (self.pool_host, self.pool_host + self.pool_size as usize);
        if at == 0 || at < lo || at >= hi || size > (hi - at) as u64 {
            return Err(Errno(libc::EINVAL));
        }
        Ok(((at - lo) as u64, info))
    }

    fn attach(&self, id: u32, res: u32, on: bool) -> Result<(), Errno> {
        if !d2k::context_exists(id) || !d2k::resource_exists(res) {
            return Err(Errno(libc::EINVAL));
        }
        let chunked = self.chunked();
        let views = match chunked.get(&res).filter(|_| on) {
            Some(chunks) => gpu3d::chunk_views(chunks),
            None => Vec::new(),
        };
        d2k::attach(id, res, on, &views)
    }

    fn unref(&self, res: u32) {
        if !d2k::resource_exists(res) {
            return;
        }
        d2k::unref(res);
        self.chunked().remove(&res);
    }

    fn set_retire(&self, retire: Retire) {
        if let Some(s) = st().as_mut() {
            s.retire = Some(retire);
        }
    }
}

/// The stream as 32-bit words, when it lies aligned (the device's copy does).
fn bytemuck_words(b: &[u8]) -> Option<&[u32]> {
    // SAFETY: any bytes are valid u32s; align_to only gives a middle part that is aligned.
    let (head, words, tail) = unsafe { b.align_to::<u32>() };
    (head.is_empty() && tail.is_empty()).then_some(words)
}

/// The level's settings (policy.rs; xnuhost.conf gpu-level=, gpu-priority=, which the session turns into these
/// variables): the mode, the name lists, and the ratio of KGSL's top clock to its lowest (its own list of the levels a
/// client may be at, read once; 4.0 when it cannot be read).
fn level_start(s: &mut St) {
    let mode = std::env::var("ONEIOS_GPU_LEVEL").ok();
    let m = match mode.as_deref() {
        Some("hold") => Mode::Hold,
        Some("governor") => Mode::Governor,
        _ => Mode::Deadline,
    };
    let (mut top, mut bottom) = (0u64, 0u64);
    if let Ok(text) = xh_core::fs::read_bytes("/sys/class/kgsl/kgsl-3d0/gpu_available_frequencies", 511) {
        let text = String::from_utf8_lossy(&text);
        for hz in text.split(|c: char| !c.is_ascii_digit()).filter(|w| !w.is_empty()) {
            let hz: u64 = hz.parse().unwrap_or(u64::MAX);
            top = top.max(hz);
            if hz != 0 && (bottom == 0 || hz < bottom) {
                bottom = hz;
            }
        }
    }
    s.pol = Policy::new(m);
    s.pol.down_at_bottom = DOWN_AT_BOTTOM.load(Ordering::Relaxed);
    if top != 0 && bottom != 0 && top > bottom {
        s.pol.ratio_x16 = (top * 16 / bottom) as u32;
    }
    s.background = std::env::var("ONEIOS_GPU_BACKGROUND").ok();
    s.front = std::env::var("ONEIOS_GPU_FRONT").ok();
    let priority = std::env::var("ONEIOS_GPU_PRIORITY").ok();
    let off = priority.as_deref() == Some("off");
    s.priority_on = !off; // as patch 0008 reads it
    d2k::set_level(false); // drm2kgsl's level is ours from now on (patch 0008)
    say!(
        1,
        "gpu: level: {}; KGSL's clocks {}-{} MHz (top/lowest x{}.{:02}); priority by the owner's class {}; background by name \
         (never on screen): {}",
        match m {
            Mode::Hold => "hold (every render-server frame at the top, to 150 ms after it: gpu-level=hold)",
            Mode::Governor => "the governor's alone (gpu-level=governor)",
            Mode::Deadline =>
                "by deadline (the top only for a frame that would miss 8.3 ms at 120 Hz, or its work interval's deadline, \
                 at the governor's clock)",
        },
        bottom / 1_000_000,
        top / 1_000_000,
        s.pol.ratio_x16 / 16,
        s.pol.ratio_x16 % 16 * 100 / 16,
        if off {
            "off (gpu-priority=0)"
        } else if UI_RB.load(Ordering::Relaxed) {
            "on (render server and front 4, default and background 8; background at the lowest level at a still screen)"
        } else {
            "on (all classes 8, out of Android UI ringbuffer; lowest level on idle background or display off)"
        },
        s.background.as_deref().unwrap_or(policy::DEFAULT_BACKGROUND)
    );
}

/// gpu3d::kgsl_open's renderer (see there).
pub fn open(
    ram_fd: RawFd,
    ram_phys: u64,
    ram_size: u64,
    pool_host: *mut u8,
    pool_pa: u64,
    pool_size: u64,
    log: LogFn,
) -> Result<Box<dyn Gpu3d>, String> {
    if st().is_some() {
        return Err("the renderer is already open".into());
    }
    SINK.store(log as usize, Ordering::Release);
    for (i, node) in [c"/dev/kgsl-3d0", c"/dev/udmabuf", c"/dev/dma_heap/system"].iter().enumerate() {
        let flags = if i == 2 { libc::O_RDONLY } else { libc::O_RDWR } | libc::O_CLOEXEC;
        if let Err(e) = d2k::sys::open(node, flags) {
            return Err(format!("{}: {}", node.to_string_lossy(), xh_core::sys::error_text(&e)));
        }
    }
    // Every guest BO is one fd per udmabuf chunk here and one more in each context that imports it, and every Metal
    // client is a KGSL fd and a dma-heap fd: with many clients the soft limit of open files is what runs out first
    // (Droid-VM's crosvm ran out of fds under GFXBench and raised it the same way). Up to the hard limit, which needs no
    // privilege.
    let mut files = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: reads and sets this process's RLIMIT_NOFILE through our struct.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut files) == 0 && files.rlim_cur < files.rlim_max {
            let was = files.rlim_cur;
            files.rlim_cur = files.rlim_max;
            if libc::setrlimit(libc::RLIMIT_NOFILE, &files) == 0 {
                say!(1, "gpu: open files: soft limit {} -> {}", was, files.rlim_max);
            }
        }
    }
    let (chunk_bytes, chunk_runs) = gpu3d::udmabuf_limits();
    // One udmabuf of one page from the pool: the memfd is sealed against shrinking and the kernel takes it.
    if let Err(e) = gpu3d::udmabuf(ram_fd, ram_phys, ram_size, &[Entry::new(pool_pa, 4096)]) {
        return Err(format!("udmabuf over the RAM memfd: {e}"));
    }
    if pool_pa < ram_phys || pool_size > ram_size || pool_pa - ram_phys > ram_size - pool_size || pool_size <= 2 << 20 {
        // drm2kgsl never hands out its first 2 MiB
        return Err("the pool must lie in the RAM and be larger than 2 MiB".into());
    }
    std::env::set_var("CROSVM_DRM2KGSL_ARENA_FD", ram_fd.to_string());
    std::env::set_var("CROSVM_DRM2KGSL_ARENA_FD_OFFSET", (pool_pa - ram_phys).to_string());
    std::env::set_var("CROSVM_DRM2KGSL_ARENA_HOST_VA", (pool_host as usize as u64).to_string());
    std::env::set_var("CROSVM_DRM2KGSL_ARENA_SIZE", pool_size.to_string());
    if std::env::var_os("CROSVM_DRM2KGSL_DIAG").is_none() {
        std::env::set_var("CROSVM_DRM2KGSL_DIAG", "0"); // its periodic counters: on request only
    }
    match d2k::init(renderer_line, fence_retired) {
        Ok(()) => {}
        Err(Errno(libc::EEXIST)) => {}
        Err(_) => return Err("no memory for the renderer's tables".into()),
    }
    let probe = d2k::sys::open(c"/dev/kgsl-3d0", libc::O_RDWR | libc::O_CLOEXEC)
        .map_err(|e| Errno(e.raw_os_error().unwrap_or(libc::EIO)))
        .and_then(|fd| d2k::probe(fd.as_fd())); // keeps its own descriptor for the VBO
    let capset = match probe {
        Ok(c) => c,
        Err(Errno(e)) => {
            return Err(format!(
                "KGSL refused the probe ({}): no device info, or no virtual buffer object (KGSL_MEMFLAGS_VBO)",
                -e
            ))
        }
    };
    let caps = capset.caps;
    let mut s = St {
        ctx: Box::new([StatCtx::EMPTY; STAT_CTX]),
        since: 0,
        t: Totals::default(),
        holds: 0,
        keepalives: 0,
        dropped: 0,
        plain: 0,
        unreadable: 0,
        imports_said: false,
        pol: Policy::new(Mode::Deadline),
        background: None,
        front: None,
        lowest_n: 0,
        said: Said::default(),
        priority_on: true,
        fg_pending: 0,
        pages: Pages::default(),
        retire: None,
        context_lines: 0,
    };
    level_start(&mut s);
    *st() = Some(s);
    say!(
        1,
        "gpu: drm2kgsl on {}: chip 0x{:x} (gpu id {}), GPU addresses 0x{:x} + {} GiB, {} priorities; pool {} MiB; guest pages \
         in udmabufs of up to {} MiB and {} runs",
        d2k::DEVICE_NODE,
        caps.chip_id,
        caps.gpu_id,
        caps.va_start,
        caps.va_size >> 30,
        caps.priorities,
        pool_size >> 20,
        chunk_bytes >> 20,
        chunk_runs
    );
    Ok(Box::new(Kgsl {
        capset,
        ram_fd,
        ram_phys,
        ram_size,
        pool_host: pool_host as usize,
        pool_size,
        chunked: Mutex::new(HashMap::new()),
    }))
}

// The tests run the renderer against a stand-in KGSL through drm2kgsl-sys's host-only open/ioctl table (and glibc's
// mallinfo2 for the leak check): host builds only. On the phone the renderer meets the real KGSL (gpuctl).
#[cfg(all(test, not(target_os = "android")))]
mod tests;
