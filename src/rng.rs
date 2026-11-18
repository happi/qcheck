//! The random source handed to generators.

/// A seeded random source plus a size hint.
///
/// The size starts at zero and grows during a run, so early tests use small
/// values and later tests use larger ones.
pub struct Gen {
    state: u64,
    size: usize,
}

impl Gen {
    pub fn new(seed: u64, size: usize) -> Self {
        Gen { state: seed, size }
    }

    /// Upper bound generators should respect for lengths and magnitudes.
    pub fn size(&self) -> usize {
        self.size
    }

    pub(crate) fn set_size(&mut self, size: usize) {
        self.size = size;
    }

    /// Next 64 random bits (SplitMix64).
    pub fn u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n`. Panics when `n` is zero.
    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0, "below(0) has no values to pick from");
        ((self.u64() as u128 * n as u128) >> 64) as u64
    }

    /// A value in `lo..=hi`. The range may span at most 2^64 values.
    pub fn int_in(&mut self, lo: i128, hi: i128) -> i128 {
        assert!(lo <= hi, "empty range");
        let span = (hi - lo) as u128 + 1;
        assert!(span <= 1 << 64, "range wider than 64 bits");
        lo + ((self.u64() as u128 * span) >> 64) as i128
    }

    pub fn bool(&mut self) -> bool {
        self.u64() & 1 == 1
    }

    /// One element of a non-empty slice.
    pub fn choose<T: Clone>(&mut self, items: &[T]) -> T {
        items[self.below(items.len() as u64) as usize].clone()
    }
}
