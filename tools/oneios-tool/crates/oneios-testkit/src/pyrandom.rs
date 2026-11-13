//! Python's `random.Random(seed)` for a small non-negative seed, as far as the helpers use it: the same numbers
//! as CPython, so that a test's sample and its streams are the ones the Python helpers made. The generator is
//! MT19937 from the `rand_mt` crate; what CPython builds on it (`Lib/random.py`, `Modules/_randommodule.c`) is here.

pub struct PyRandom(rand_mt::Mt);

impl PyRandom {
    pub fn new(seed: u32) -> PyRandom {
        PyRandom(rand_mt::Mt::new_with_key([seed]))
    }

    /// `getrandbits(32)`.
    pub fn bits32(&mut self) -> u32 {
        self.0.next_u32()
    }

    /// `random()`: 53 bits from two words.
    pub fn random(&mut self) -> f64 {
        let a = (self.0.next_u32() >> 5) as f64;
        let b = (self.0.next_u32() >> 6) as f64;
        (a * 67108864.0 + b) * (1.0 / 9007199254740992.0)
    }

    /// `_randbelow_with_getrandbits(n)`, n > 0 and below 2^32: `randrange(n)` and the index of `choice`.
    pub fn below(&mut self, n: u64) -> u64 {
        let k = 64 - n.leading_zeros();
        loop {
            let r = (self.0.next_u32() >> (32 - k)) as u64;
            if r < n {
                return r;
            }
        }
    }

    /// `choice(seq)`.
    pub fn choice<T: Copy>(&mut self, seq: &[T]) -> T {
        seq[self.below(seq.len() as u64) as usize]
    }

    /// `choices(population, k=k)`: `floor(random() * n)` each time.
    pub fn choices(&mut self, population: &[u8], k: usize) -> Vec<u8> {
        let n = population.len() as f64;
        (0..k).map(|_| population[(self.random() * n).floor() as usize]).collect()
    }

    /// `randbytes(n)`: `getrandbits(8 n)` as little-endian bytes. Whole words first; the last word gives its
    /// top bits when fewer than four bytes are left.
    pub fn randbytes(&mut self, n: usize, out: &mut Vec<u8>) {
        for _ in 0..n / 4 {
            out.extend_from_slice(&self.0.next_u32().to_le_bytes());
        }
        let rest = n % 4;
        if rest != 0 {
            let word = self.0.next_u32() >> (32 - 8 * rest);
            out.extend_from_slice(&word.to_le_bytes()[..rest]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // python3 -c "import random; r=random.Random(27); print(r.getrandbits(32), repr(r.random()), r.randrange(1000),
    //   r.choice([4,8,16]), bytes(r.choices(b'abcdef', k=5)).decode(), r.randbytes(7).hex())"
    // 2785274337 0.4798819102448936 283 8 baebc f10b9bd0902467   (CPython 3.14, 2026-11-13)
    #[test]
    fn the_numbers_are_cpythons() {
        let mut r = PyRandom::new(27);
        assert_eq!(r.bits32(), 2785274337);
        assert_eq!(r.random(), 0.4798819102448936);
        assert_eq!(r.below(1000), 283);
        assert_eq!(r.choice(&[4, 8, 16]), 8);
        assert_eq!(r.choices(b"abcdef", 5), b"baebc");
        let mut bytes = Vec::new();
        r.randbytes(7, &mut bytes);
        assert_eq!(bytes, [0xf1, 0x0b, 0x9b, 0xd0, 0x90, 0x24, 0x67]);
    }
}
