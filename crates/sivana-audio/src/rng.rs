//! Seeded xorshift64* PRNG.
//!
//! Deliberately not `rand`: benchmark fixtures must be reproducible across
//! dependency upgrades and platforms (PLAN.md §36). This generator is tiny,
//! deterministic and good enough for synthetic audio and noise.

/// State for the xorshift64* generator. Zero is an invalid seed.
#[derive(Debug, Clone)]
pub struct XorShift64Star {
    state: u64,
}

impl XorShift64Star {
    /// Create a generator from a non-zero seed.
    ///
    /// # Panics
    /// Panics if `seed == 0` (xorshift cannot recover from an all-zero state).
    pub fn new(seed: u64) -> Self {
        assert!(seed != 0, "xorshift64* seed must be non-zero");
        Self { state: seed }
    }

    /// Derive a sub-seed — useful for generating many fixtures from one root seed.
    pub fn derive(&self, salt: u64) -> Self {
        let mut mixed = self.state ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        if mixed == 0 {
            mixed = 0x9E37_79B9_7F4A_7C15;
        }
        Self { state: mixed }
    }

    /// Next raw 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform `f32` in `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform `f32` in `[-1, 1)`.
    pub fn next_bipolar(&mut self) -> f32 {
        self.next_f32() * 2.0 - 1.0
    }
}

impl Default for XorShift64Star {
    fn default() -> Self {
        Self::new(0x5163_3E2D_3F17_A1B9)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = XorShift64Star::new(42);
        let mut b = XorShift64Star::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn output_range_and_nonzero() {
        let mut rng = XorShift64Star::default();
        let mut any_high = false;
        for _ in 0..10_000 {
            let v = rng.next_f32();
            assert!((0.0..1.0).contains(&v));
            if v > 0.999 { any_high = true; }
        }
        assert!(any_high, "generator appears stuck");
    }

    #[test]
    fn derive_changes_stream() {
        let root = XorShift64Star::new(7);
        let mut a = root.derive(1);
        let mut b = root.derive(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn zero_seed_panics() {
        assert!(std::panic::catch_unwind(|| XorShift64Star::new(0)).is_err());
    }
}
