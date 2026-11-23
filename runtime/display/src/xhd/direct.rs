// SPDX-License-Identifier: MIT
//! Direct frames: the helper copies the guest's scanout from guest RAM itself (xnuhost's "memory" and "frame"
//! messages, runtime/xnuhost/src/display.rs).
use super::protocol::Message;
use super::Rect;
use crate::sys::{self, Errno, Map, Result};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use xh_core::bytes::le;

/// A slot of the direct table, as xnuhost writes it (struct xhd_slot, 64 bytes): the frame `seq`, rows of `stride`
/// bytes from byte `offset` of the segments (the guest's pages as offsets into the RAM memfd, in the scanout's order),
/// `width` x `height`, the rectangle x, y, w, h changed, format 1 (B,G,R,X/A). seq is written last. The slot's
/// `nsegs` segments ([`Seg`]) follow it in the table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Slot {
    pub seq: u64,
    pub offset: u64,
    pub stride: u32,
    pub width: u32,
    pub height: u32,
    pub nsegs: u32,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub format: u32,
}

impl Slot {
    pub const BYTES: usize = 64;
    /// The byte offset of seq in a slot (written last, with release, by xnuhost).
    pub const SEQ_OFFSET: usize = 0;

    pub fn decode(b: &[u8]) -> Slot {
        Slot {
            seq: le::u64(b, 0),
            offset: le::u64(b, 8),
            stride: le::u32(b, 16),
            width: le::u32(b, 20),
            height: le::u32(b, 24),
            nsegs: le::u32(b, 28),
            x: le::u32(b, 32),
            y: le::u32(b, 36),
            w: le::u32(b, 40),
            h: le::u32(b, 44),
            format: le::u32(b, 48),
        }
    }

    /// The slot's 64 bytes (the padding zero).
    pub fn encode(&self) -> [u8; Slot::BYTES] {
        let mut out = [0u8; Slot::BYTES];
        out[0..8].copy_from_slice(&self.seq.to_le_bytes());
        out[8..16].copy_from_slice(&self.offset.to_le_bytes());
        let words = [self.stride, self.width, self.height, self.nsegs, self.x, self.y, self.w, self.h, self.format];
        for (i, v) in words.into_iter().enumerate() {
            out[16 + i * 4..20 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        out
    }

    fn changed(&self) -> Rect {
        Rect::new(self.x, self.y, self.w, self.h)
    }
}

/// A segment of a slot: `length` bytes at `offset` of the RAM memfd (struct xhd_seg, 16 bytes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Seg {
    pub offset: u64,
    pub length: u64,
}

impl Seg {
    pub const BYTES: usize = 16;

    pub fn decode(b: &[u8]) -> Seg {
        Seg { offset: le::u64(b, 0), length: le::u64(b, 8) }
    }

    pub fn encode(&self) -> [u8; Seg::BYTES] {
        let mut out = [0u8; Seg::BYTES];
        out[..8].copy_from_slice(&self.offset.to_le_bytes());
        out[8..].copy_from_slice(&self.length.to_le_bytes());
        out
    }
}

/// The newest frame xnuhost sent: that slot, that seq, and its `sent` (0: not known).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingFrame {
    pub slot: u32,
    pub seq: u64,
    pub sent_us: u32,
}

/// Guest RAM and the table, both mapped read-only.
struct Memory {
    ram: Map,
    table: Map,
    slots: u32,
    slot_bytes: u32,
    _fds: Vec<OwnedFd>,
}

/// The helper's end of direct frames: the maps (none until xnuhost's "memory") and the pending frame.
#[derive(Default)]
pub struct Direct {
    memory: Option<Memory>,
    /// A frame came: the newest.
    pub pending: Option<PendingFrame>,
    /// xnuhost's options (OPTION_*), 0 until an "options" message says otherwise.
    pub options: u32,
}

impl Direct {
    pub fn new() -> Direct {
        Direct::default()
    }

    /// Whether xnuhost's "memory" came and was mapped.
    pub fn is_mapped(&self) -> bool {
        self.memory.is_some()
    }

    pub fn ram_size(&self) -> u64 {
        self.memory.as_ref().map_or(0, |m| m.ram.len() as u64)
    }

    pub fn slots(&self) -> u32 {
        self.memory.as_ref().map_or(0, |m| m.slots)
    }

    /// xnuhost's "memory" with its descriptors (the RAM memfd and the table memfd). Only the first usable one is
    /// taken; anything else is dropped (its descriptors closed).
    pub(crate) fn take_memory(&mut self, m: &Message, fds: Vec<OwnedFd>) {
        let ram = Message::wide(m.a, m.b);
        let (slots, slot_bytes) = (m.c, m.d);
        if self.memory.is_some()
            || fds.len() != 2
            || slots == 0
            || slots > 64
            || (slot_bytes as usize) < Slot::BYTES
            || slot_bytes > 64 << 20
            || slot_bytes % 8 != 0
            || ram == 0
        {
            return;
        }
        let (Ok((ram_file, _)), Ok((table_file, _))) = (sys::file_size(fds[0].as_fd()), sys::file_size(fds[1].as_fd()))
        else {
            return;
        };
        let table_bytes = slots as u64 * slot_bytes as u64;
        if (ram_file as u64) < ram || (table_file as u64) < table_bytes {
            return;
        }
        let Ok(ram_map) = Map::new(fds[0].as_fd(), ram as usize, false) else { return };
        let Ok(table) = Map::new(fds[1].as_fd(), table_bytes as usize, false) else { return };
        self.memory = Some(Memory { ram: ram_map, table, slots, slot_bytes, _fds: fds });
    }

    /// xnuhost's "frame": the newest frame wins (an older seq, or a slot past the table, changes nothing).
    pub(crate) fn offer(&mut self, slot: u32, seq: u64, sent_us: u32) {
        if self.memory.as_ref().is_some_and(|m| slot < m.slots) && self.pending.is_none_or(|p| seq > p.seq) {
            self.pending = Some(PendingFrame { slot, seq, sent_us });
        }
    }

    /// The rows of the pending frame's slot.
    pub(crate) fn walk(&self) -> Result<Walk<'_>> {
        let (Some(mem), Some(p)) = (&self.memory, self.pending) else { return Err(Errno(libc::EINVAL)) };
        if p.slot >= mem.slots {
            return Err(Errno(libc::EINVAL));
        }
        let table = mem.table.bytes();
        let base = p.slot as usize * mem.slot_bytes as usize;
        let s = Slot::decode(&table[base..base + Slot::BYTES]);
        // SAFETY: base is a multiple of slot_bytes (a multiple of 8) in a page-aligned map, so the seq word is aligned;
        // xnuhost writes it with a release store, read here with acquire.
        let seq =
            unsafe { &*(mem.table.as_ptr().add(base + Slot::SEQ_OFFSET) as *const AtomicU64) }.load(Ordering::Acquire);
        if seq != p.seq || s.seq != p.seq {
            return Err(Errno(libc::ESTALE)); // reused already
        }
        let max_segs = (mem.slot_bytes as usize - Slot::BYTES) / Seg::BYTES;
        if s.format != 1
            || s.width == 0
            || s.height == 0
            || (s.stride as u64) < s.width as u64 * 4
            || s.nsegs as usize > max_segs
        {
            return Err(Errno(libc::EINVAL));
        }
        Ok(Walk { ram: mem.ram.bytes(), segs: &table[base + Slot::BYTES..], s, at: 0, start: 0 })
    }

    /// Copy a rectangle of the pending frame (`only`, or None: the slot's changed rectangle; clipped to the frame and
    /// to dst_width x dst_height) from guest RAM into a B,G,R,X/A buffer (swap: R,G,B,X, alpha 0xff). The rectangle
    /// copied, or the error: a slot that changed (ESTALE) or does not fit RAM (EINVAL) copies nothing more.
    pub fn copy_direct(
        &self,
        dst: &mut [u8],
        dst_stride: usize,
        dst_width: u32,
        dst_height: u32,
        swap: bool,
        only: Option<Rect>,
    ) -> Result<Rect> {
        let mut k = self.walk()?;
        let mut r = only.unwrap_or_else(|| k.s.changed());
        if !r.clip(k.s.width, k.s.height) || !r.clip(dst_width, dst_height) {
            return Ok(Rect::EMPTY);
        }
        for row in r.y..r.y + r.h {
            let to = &mut dst[row as usize * dst_stride + r.x as usize * 4..][..r.w as usize * 4];
            k.row_into(row, r.x, to)?;
            if swap {
                super::swap_bgrx_in_place(to);
            }
        }
        Ok(r)
    }
}

/// A cursor over a slot's segments, for rows that only go forward.
pub(crate) struct Walk<'a> {
    ram: &'a [u8],
    segs: &'a [u8],
    pub(crate) s: Slot,
    at: u32,
    start: u64,
}

impl Walk<'_> {
    fn seg(&self, i: u32) -> Seg {
        Seg::decode(&self.segs[i as usize * Seg::BYTES..][..Seg::BYTES])
    }

    /// Pixels [x, x + to.len() / 4) of a row into `to`, in order. EINVAL when the row reaches past the segments or a
    /// segment past RAM. (Sums that would wrap are refused as well: the C let them wrap.)
    pub(crate) fn row_into(&mut self, row: u32, x: u32, to: &mut [u8]) -> Result<()> {
        let invalid = Errno(libc::EINVAL);
        let want = (row as u64)
            .checked_mul(self.s.stride as u64)
            .and_then(|v| v.checked_add(self.s.offset))
            .and_then(|v| v.checked_add(x as u64 * 4))
            .ok_or(invalid)?;
        while self.at < self.s.nsegs {
            let end = self.start.checked_add(self.seg(self.at).length).ok_or(invalid)?;
            if want < end {
                break;
            }
            self.start = end;
            self.at += 1;
        }
        let ram_size = self.ram.len() as u64;
        let (mut i, mut i_start, mut pos, mut done) = (self.at, self.start, want, 0usize);
        while done < to.len() {
            if i >= self.s.nsegs {
                return Err(invalid);
            }
            let g = self.seg(i);
            if g.offset > ram_size || g.length > ram_size - g.offset || pos < i_start {
                return Err(invalid);
            }
            let inside = (pos - i_start).min(g.length);
            let n = ((g.length - inside) as usize).min(to.len() - done);
            let from = (g.offset + inside) as usize;
            to[done..done + n].copy_from_slice(&self.ram[from..from + n]);
            done += n;
            pos += n as u64;
            if done < to.len() {
                i_start = i_start.checked_add(g.length).ok_or(invalid)?;
                i += 1;
            }
        }
        Ok(())
    }
}
