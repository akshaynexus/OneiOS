//! Two pieces of CPython that align.py's output depends on, so that the Rust step writes the same file:
//! the iteration order of a `set` of integers, and `random.Random(seed).sample`.
//!
//! No library gives either. The set order is CPython's own table layout (Objects/setobject.c: open addressing,
//! 9 linear probes, then the perturbed jump; resized at 3/5 full), stable since 3.7 and checked here against
//! Python 3.14 on the real inputs (tests/prelink/compare-python.sh: align's holdout sample is drawn from a list
//! made from a set). The generator is MT19937 from the `rand_mt` crate; only `sample`'s index choice is ours.

const LINEAR_PROBES: usize = 9;
const PERTURB_SHIFT: u32 = 5;
const MINSIZE: usize = 8;

/// `hash(n)` of a non-negative Python int.
fn hash_int(n: u64) -> u64 {
    n % ((1u64 << 61) - 1)
}

/// A CPython `set` of non-negative ints: add, membership, and iteration in the table's order. No removal.
pub struct PySet {
    table: Vec<Option<u64>>,
    mask: usize,
    fill: usize,
}

impl Default for PySet {
    fn default() -> Self {
        PySet { table: vec![None; MINSIZE], mask: MINSIZE - 1, fill: 0 }
    }
}

impl PySet {
    /// `set(d)` for a dict `d` with these keys in this order (one resize at the start, as set_update_internal does
    /// for a dict), or `set()` followed by `add` for anything else (`from_adds`).
    pub fn from_dict_keys(keys: impl ExactSizeIterator<Item = u64>) -> PySet {
        let mut set = PySet::default();
        let n = keys.len();
        if (set.fill + n) * 5 >= set.mask * 3 {
            set.resize(n * 2);
        }
        for key in keys {
            set.add(key);
        }
        set
    }

    pub fn from_adds(keys: impl Iterator<Item = u64>) -> PySet {
        let mut set = PySet::default();
        for key in keys {
            set.add(key);
        }
        set
    }

    fn resize(&mut self, minused: usize) {
        let mut newsize = MINSIZE;
        while newsize <= minused {
            newsize <<= 1;
        }
        let old = std::mem::replace(&mut self.table, vec![None; newsize]);
        self.mask = newsize - 1;
        for key in old.into_iter().flatten() {
            // set_insert_clean
            let hash = hash_int(key);
            let (mut perturb, mut i) = (hash, hash as usize & self.mask);
            'placed: loop {
                if self.table[i].is_none() {
                    self.table[i] = Some(key);
                    break;
                }
                if i + LINEAR_PROBES <= self.mask {
                    for j in 1..=LINEAR_PROBES {
                        if self.table[i + j].is_none() {
                            self.table[i + j] = Some(key);
                            break 'placed;
                        }
                    }
                }
                perturb >>= PERTURB_SHIFT;
                i = (i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb as usize)) & self.mask;
            }
        }
    }

    pub fn add(&mut self, key: u64) {
        let hash = hash_int(key);
        let (mut perturb, mut i) = (hash, hash as usize & self.mask);
        loop {
            let probes = if i + LINEAR_PROBES <= self.mask { LINEAR_PROBES } else { 0 };
            for j in 0..=probes {
                match self.table[i + j] {
                    None => {
                        self.table[i + j] = Some(key);
                        self.fill += 1;
                        if self.fill * 5 >= self.mask * 3 {
                            self.resize(if self.fill > 50000 { self.fill * 2 } else { self.fill * 4 });
                        }
                        return;
                    }
                    Some(have) if have == key => return,
                    Some(_) => {}
                }
            }
            perturb >>= PERTURB_SHIFT;
            i = (i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb as usize)) & self.mask;
        }
    }

    /// The keys in the order `for key in the_set` gives them.
    pub fn iter(&self) -> impl Iterator<Item = u64> + '_ {
        self.table.iter().flatten().copied()
    }
}

/// `random.Random(seed)` for a small non-negative seed: `sample` only.
pub struct PyRandom(rand_mt::Mt);

impl PyRandom {
    pub fn new(seed: u32) -> PyRandom {
        PyRandom(rand_mt::Mt::new_with_key([seed]))
    }

    /// `_randbelow_with_getrandbits(n)`, n > 0 and below 2^32.
    fn below(&mut self, n: usize) -> usize {
        let k = usize::BITS - n.leading_zeros();
        loop {
            let r = (self.0.next_u32() >> (32 - k)) as usize;
            if r < n {
                return r;
            }
        }
    }

    /// `rng.sample(population, k)`.
    pub fn sample<T: Copy>(&mut self, population: &[T], k: usize) -> Vec<T> {
        let n = population.len();
        assert!(k <= n && n < 1 << 32);
        let mut result = Vec::with_capacity(k);
        let mut setsize = 21usize;
        if k > 5 {
            setsize += 4usize.pow(((k as f64 * 3.0).ln() / 4f64.ln()).ceil() as u32);
        }
        if n <= setsize {
            let mut pool = population.to_vec();
            for i in 0..k {
                let j = self.below(n - i);
                result.push(pool[j]);
                pool[j] = pool[n - i - 1];
            }
        } else {
            let mut selected = std::collections::HashSet::new();
            for _ in 0..k {
                let mut j = self.below(n);
                while !selected.insert(j) {
                    j = self.below(n);
                }
                result.push(population[j]);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The expected values are CPython 3.14's: list(set(d)), list of a set built with add, random.Random(2).sample.
    #[test]
    fn set_order_small() {
        let keys = [0xfffffe0007a01234u64, 0xfffffe0007a01238, 5, 13, 21, 29, 37, 0xfffffff007004000, 8, 16, 1 << 40];
        let got: Vec<u64> = PySet::from_adds(keys.iter().copied()).iter().collect();
        assert_eq!(got, EXPECT_ADDS);
        let got: Vec<u64> = PySet::from_dict_keys(keys.iter().copied()).iter().collect();
        assert_eq!(got, EXPECT_DICT);
    }
    const EXPECT_ADDS: [u64; 11] = include!("pyset_adds.in");
    const EXPECT_DICT: [u64; 11] = include!("pyset_dict.in");

    #[test]
    fn sample_is_pythons() {
        let population: Vec<u32> = (0..100).collect();
        assert_eq!(PyRandom::new(2).sample(&population, 7), include!("pyset_sample_pool.in"));
        let population: Vec<u32> = (0..100000).collect();
        assert_eq!(PyRandom::new(2).sample(&population, 6), include!("pyset_sample_set.in"));
    }
}
