//! `oneios-tool debug mkstream` (was tests/phone/mkstream.py): the streams of tests/phone/sparse-compare.sh, to
//! stdout; the same bytes on every run, and the bytes mkstream.py wrote (Python's random.Random(0x0E105):
//! pyrandom.rs).
//!
//!   debug mkstream runs             zero runs and data runs shorter than 4 KiB, between 4 KiB and 4 MiB, and longer
//!                                   (20 and 70 MiB of zeros too: APFS keeps a hole only from about 16 MiB), on the
//!                                   edges of blocks and off them
//!   debug mkstream tail             data, then zeros that end 123 bytes into a block
//!   debug mkstream zero             9 MiB and 123 bytes of zeros
//!   debug mkstream empty            nothing
//!   debug mkstream lastbyte         33 MiB and 1000 bytes of zeros, the last byte not zero
//!   debug mkstream dense            5 MiB and 7 bytes without a zero run
//!   debug mkstream phone MIB DATA   a guess at the shape of the phone's Data store: MIB MiB, of which about DATA
//!                                   MiB hold data, in runs of 4 KiB to tens of MiB with short holes inside; half
//!                                   of the data does not compress (the real store is 32768 MiB with about 2100
//!                                   of data)

use std::io::{self, Write};

use crate::pyrandom::PyRandom;

const KIB: u64 = 1 << 10;
const MIB: u64 = 1 << 20;

struct Stream<W: Write> {
    out: W,
    rng: PyRandom,
    zeros: Vec<u8>,
}

impl<W: Write> Stream<W> {
    fn zeros(&mut self, mut n: u64) -> io::Result<()> {
        while n > 0 {
            let m = n.min(MIB);
            self.out.write_all(&self.zeros[..m as usize])?;
            n -= m;
        }
        Ok(())
    }

    /// n bytes with no zero: random bytes (0 mapped to 1), or one line of text again and again (gzip packs it
    /// small). Which of the two is drawn once for the run.
    fn data(&mut self, mut n: u64) -> io::Result<()> {
        let compressible = self.rng.random() < 0.5;
        let mut chunk = Vec::with_capacity(MIB as usize);
        while n > 0 {
            let m = n.min(MIB) as usize;
            chunk.clear();
            if compressible {
                let number = self.rng.bits32();
                let mut line = format!("{number:08x} ").into_bytes();
                line.extend(self.rng.choices(b"abcdefghijklmnopqrstuvwxyz ", 40));
                line.push(b'\n');
                while chunk.len() < m {
                    chunk.extend_from_slice(&line);
                }
            } else {
                self.rng.randbytes(m, &mut chunk);
                chunk.iter_mut().filter(|b| **b == 0).for_each(|b| *b = 1);
            }
            self.out.write_all(&chunk[..m])?;
            n -= m as u64;
        }
        Ok(())
    }

    fn runs(&mut self) -> io::Result<()> {
        self.data(1)?; // one byte, then
        for (z, d) in [
            (100, 50), // zero runs shorter than a block, inside one
            (4095, 1),
            (4096, 4096),
            (4097, 4095),
            (3000, 10000),
            (16384, 16384),
            (16385, 3),
            (65536 - 20000, 70000), // a few blocks, not on an edge
            (MIB, MIB),
            (MIB + 1, 5),
            (4 * MIB - 1, 4 * MIB + 1), // just under the script's 4 MiB
            (4 * MIB, 4 * MIB),
            (8 * MIB + 4096, 9 * MIB + 77),
            (15 * MIB, 4096),
            (20 * MIB, 123456),
            (70 * MIB, 8192),
            (12345, 1),
        ] {
            self.zeros(z)?;
            self.data(d)?;
        }
        self.zeros(6 * MIB) // and a long run of zeros at the end
    }

    fn phone(&mut self, mib: u64, data_mib: u64) -> io::Result<()> {
        let (size, want) = (mib * MIB, data_mib * MIB);
        let regions = (data_mib / 4).max(8);
        // each region: where it starts (a 4 KiB edge) and how long it is
        let mut starts: Vec<u64> = (0..regions).map(|_| self.rng.below(size / 4096) * 4096).collect();
        starts.sort_unstable();
        let mut at = 0;
        for (i, &start) in starts.iter().enumerate() {
            let end = starts.get(i + 1).copied().unwrap_or(size);
            let share = self.rng.choice(&[0.01, 0.05, 0.25, 0.5, 1.0, 1.0, 2.0, 4.19]);
            let length = (want as f64 / regions as f64 * share) as u64;
            let length = (length.min(end - start) / 4096 * 4096).max(4096);
            // Two regions that start on one edge: the first one's 4 KiB reach past the second's start, and the
            // Python wrote no zeros then (a negative count) and went on from the start, as here.
            self.zeros(start.saturating_sub(at))?;
            at = start;
            while at < start + length {
                let n = (start + length - at).min(self.rng.choice(&[4, 8, 16, 64, 256, 1024, 4096]) * KIB);
                self.data(n)?;
                at += n;
                if at < start + length {
                    // a short hole inside the region
                    let h = (start + length - at).min(self.rng.choice(&[4, 4, 8, 16, 64, 512]) * KIB);
                    self.zeros(h)?;
                    at += h;
                }
            }
        }
        self.zeros(size.saturating_sub(at))
    }
}

pub const USAGE: &str = "usage: oneios-tool debug mkstream runs|tail|zero|empty|lastbyte|dense | phone MIB DATA";

pub fn run(args: &[String]) -> i32 {
    let stdout = io::stdout().lock();
    let mut s = Stream { out: io::BufWriter::with_capacity(1 << 20, stdout), rng: PyRandom::new(0x0E105), zeros: vec![0; MIB as usize] };
    let number = |i: usize| args.get(i).and_then(|a| a.parse::<u64>().ok());
    let done = match (args.first().map(String::as_str), number(1), number(2)) {
        (Some("runs"), ..) => s.runs(),
        (Some("tail"), ..) => s.data(3 * MIB + 5000).and_then(|()| s.zeros(2 * MIB + 123)),
        (Some("zero"), ..) => s.zeros(9 * MIB + 123),
        (Some("empty"), ..) => Ok(()),
        (Some("lastbyte"), ..) => s.zeros(33 * MIB + 999).and_then(|()| s.out.write_all(b"\x01")),
        (Some("dense"), ..) => s.data(5 * MIB + 7),
        (Some("phone"), Some(mib), Some(data)) => s.phone(mib, data),
        _ => {
            eprintln!("{USAGE}");
            return 1;
        }
    };
    match done.and_then(|()| s.out.flush()) {
        Ok(()) => 0,
        // The reader went away (a pipe closed early): not this helper's failure to report.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => 1,
        Err(e) => {
            eprintln!("mkstream: {e}");
            1
        }
    }
}
