// SPDX-License-Identifier: MIT
//! A small protobuf (proto2 wire format) coder for the Qualcomm SEE messages: varint, zigzag sint64, fixed32,
//! fixed64, float, length-delimited bytes, strings and nested messages. No schema: callers write fields by number and
//! walk received messages field by field.
//!
//! Ported from DenialOfGravity/ios-redfin 23019ab4 `phone/sensors/pb.c` and `pb.h` (MIT, Copyright (c) 2026
//! dipa-studio): the same functions in Rust, a writer over a Vec and a reader over a slice.

/// Wire types.
pub const VARINT: u8 = 0;
pub const FIXED64: u8 = 1;
pub const LEN: u8 = 2;
pub const FIXED32: u8 = 5;

/// A message being written.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct Writer {
    pub buf: Vec<u8>,
}

/// `v` as a varint onto `out`.
pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let c = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(c);
            return;
        }
        out.push(c | 0x80);
    }
}

impl Writer {
    pub fn new() -> Writer {
        Writer::default()
    }
    fn key(&mut self, field: u32, wire: u8) {
        put_varint(&mut self.buf, (u64::from(field) << 3) | u64::from(wire));
    }
    pub fn varint(&mut self, field: u32, v: u64) -> &mut Self {
        self.key(field, VARINT);
        put_varint(&mut self.buf, v);
        self
    }
    pub fn boolean(&mut self, field: u32, v: bool) -> &mut Self {
        self.varint(field, u64::from(v))
    }
    /// zigzag
    pub fn sint64(&mut self, field: u32, v: i64) -> &mut Self {
        self.varint(field, ((v as u64) << 1) ^ ((v >> 63) as u64))
    }
    pub fn fixed32(&mut self, field: u32, v: u32) -> &mut Self {
        self.key(field, FIXED32);
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn fixed64(&mut self, field: u32, v: u64) -> &mut Self {
        self.key(field, FIXED64);
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn float(&mut self, field: u32, v: f32) -> &mut Self {
        self.fixed32(field, v.to_bits())
    }
    /// Bytes, a string or a nested message.
    pub fn bytes(&mut self, field: u32, data: &[u8]) -> &mut Self {
        self.key(field, LEN);
        put_varint(&mut self.buf, data.len() as u64);
        self.buf.extend_from_slice(data);
        self
    }
    pub fn string(&mut self, field: u32, s: &str) -> &mut Self {
        self.bytes(field, s.as_bytes())
    }
    pub fn done(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }
}

/// One field read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field<'a> {
    pub num: u32,
    pub wire: u8,
    /// VARINT, FIXED64, FIXED32
    pub v: u64,
    /// LEN
    pub data: &'a [u8],
}

impl Field<'_> {
    /// A FIXED32 field as a float.
    pub fn f32(&self) -> f32 {
        f32::from_bits(self.v as u32)
    }
}

/// zigzag back to a signed value.
pub fn zigzag(v: u64) -> i64 {
    (v >> 1) as i64 ^ -((v & 1) as i64)
}

/// A reader of one message; `next` gives Some(Ok(field)), Some(Err(())) for a malformed message, None at the end.
pub struct Reader<'a> {
    p: &'a [u8],
    bad: bool,
}

impl<'a> Reader<'a> {
    pub fn new(p: &'a [u8]) -> Reader<'a> {
        Reader { p, bad: false }
    }

    fn varint(&mut self) -> Option<u64> {
        let mut x = 0u64;
        for s in (0..70).step_by(7) {
            let (&c, rest) = self.p.split_first()?;
            self.p = rest;
            x |= u64::from(c & 0x7f) << s;
            if c & 0x80 == 0 {
                return Some(x);
            }
        }
        None
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.p.len() < n {
            return None;
        }
        let (a, b) = self.p.split_at(n);
        self.p = b;
        Some(a)
    }

    fn field(&mut self) -> Option<Field<'a>> {
        let k = self.varint()?;
        let num = k >> 3;
        if num == 0 || num > 0x1fff_ffff {
            return None;
        }
        let wire = (k & 7) as u8;
        let mut f = Field { num: num as u32, wire, v: 0, data: &[] };
        match wire {
            VARINT => f.v = self.varint()?,
            FIXED64 => f.v = u64::from_le_bytes(self.take(8)?.try_into().ok()?),
            FIXED32 => f.v = u64::from(u32::from_le_bytes(self.take(4)?.try_into().ok()?)),
            LEN => {
                let n = usize::try_from(self.varint()?).ok()?;
                f.data = self.take(n)?;
            }
            _ => return None, // groups (3/4) are not used by SEE
        }
        Some(f)
    }
}

impl<'a> Iterator for Reader<'a> {
    type Item = Result<Field<'a>, ()>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.bad || self.p.is_empty() {
            return None;
        }
        match self.field() {
            Some(f) => Some(Ok(f)),
            None => {
                self.bad = true;
                Some(Err(()))
            }
        }
    }
}

/// Every field of a message, or None for a malformed one.
pub fn fields(p: &[u8]) -> Option<Vec<Field<'_>>> {
    Reader::new(p).collect::<Result<_, ()>>().ok()
}

/// Repeated floats: one FIXED32 field per value, or one LEN field packed. Appends to `out` up to `max` values.
pub fn floats(f: &Field, out: &mut Vec<f32>, max: usize) {
    if f.wire == FIXED32 {
        if out.len() < max {
            out.push(f.f32());
        }
    } else if f.wire == LEN {
        for c in f.data.chunks_exact(4) {
            if out.len() >= max {
                break;
            }
            out.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
        }
    }
}
