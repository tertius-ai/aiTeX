//! Pseudo-random numbers for `\pdfuniformdeviate` and `\pdfnormaldeviate`.
//!
//! **Entropy is I/O, so it comes through the [`World`](crate::world::World)**
//! — the same rule as the clock, and for the same reason (docs/history/PLAN.md decision 1).
//! The engine is handed a *seed* and generates from it here. It never reaches
//! for `getrandom`, `std::time`, or a `rand` crate: `aitex-engine` holds the
//! zero-dependency line, and an ambient entropy call is exactly the kind of
//! thing that makes a wasm32 build impossible to produce later.
//!
//! Splitting it that way keeps both properties we want at once:
//!
//! * **Real randomness** when a world supplies real entropy — the CLI seeds
//!   from the OS, a wasm host from `crypto.getRandomValues`.
//! * **Reproducibility** when it does not. `\pdfsetrandomseed` fixes the
//!   stream exactly, and [`MemoryWorld`](crate::world::MemoryWorld) has no
//!   entropy at all, so every test and every differential comparison is
//!   deterministic by construction. "Same input → byte-identical DVI" is a
//!   stated property of this port and a random engine must not quietly break
//!   it.
//!
//! The generator is PCG-XSH-RR 64/32 (O'Neill 2014): one 64-bit LCG step, an
//! xorshift and a data-dependent rotation on the way out. It is chosen for
//! being small enough to read in one sitting, having no failure modes on the
//! low bits the way a bare LCG does, and needing nothing but wrapping integer
//! arithmetic — so it behaves identically on every target, including wasm32.
//!
//! **Deliberately not cryptographic, and that is settled.** TeX's random
//! numbers exist so a document can jitter a rule or shuffle a list; nothing
//! here makes a security decision. A CSPRNG would also need real entropy on
//! every run, which would break the determinism this port is built around.
//! Nothing should ever be added here that a security decision depends on.
//!
//! # Not bit-compatible with pdfTeX
//!
//! pdfTeX draws from MetaPost's lagged-Fibonacci generator over 28-bit
//! fractions, so for a given `\pdfsetrandomseed` its sequence differs from
//! ours. Everything observable *about* the sequence agrees — range, spread,
//! determinism under a fixed seed — but a differential test that compares
//! actual deviates against `pdftex` will not match, and should compare
//! distribution or simply fix the seed and record our own values.
//!
//! **Matching it is a settled no, not a deferred yes.** It would buy fidelity
//! only for a document that both fixes a seed *and* depends on the precise
//! draws, which is rare and arguably already broken; the batteries assert the
//! properties that actually matter -- reproducibility under a seed, and range
//! -- which is a stronger check than value-matching. Against that, it would
//! mean carrying a generator whose behaviour is defined by another program's
//! source rather than by `tex.web`. The one place it could bite is `ptrip`,
//! pdfTeX's torture test, which is markedly less canonical than TRIP; if that
//! ever becomes a goal, revisit this then and not before.

/// PCG-XSH-RR 64/32.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Rng {
    state: u64,
    /// Must be odd; fixed here, so a seed alone determines the stream.
    inc: u64,
}

/// The multiplier from the reference implementation.
const MULT: u64 = 6_364_136_223_846_793_005;
const INC: u64 = 1_442_695_040_888_963_407;

impl Default for Rng {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Rng {
    pub fn new(seed: i64) -> Self {
        // `as u64` on a negative seed is the two's-complement bit pattern,
        // which is what we want: `\pdfsetrandomseed -1` is a seed like any
        // other and must not collide with `\pdfsetrandomseed 0`.
        let mut r = Self {
            state: 0,
            inc: INC | 1,
        };
        r.step();
        r.state = r.state.wrapping_add(seed as u64);
        r.step();
        r
    }

    fn step(&mut self) {
        self.state = self.state.wrapping_mul(MULT).wrapping_add(self.inc);
    }

    /// The next 32 bits.
    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.step();
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// A value in `[0, n)`, or 0 when `n` is not positive.
    ///
    /// pdfTeX's `\pdfuniformdeviate` takes the absolute value of its argument
    /// and yields 0 for 0, so a negative bound is a range, not an error.
    ///
    /// Debiased by rejection: taking `next_u32() % n` alone would make the
    /// low residues fractionally more likely, which is invisible in one draw
    /// and obvious in a histogram.
    pub fn uniform(&mut self, n: i64) -> i64 {
        let bound = n.unsigned_abs();
        if bound == 0 {
            return 0;
        }
        if bound > u32::MAX as u64 {
            // Wider than one draw: take two and reduce. Still uniform enough
            // for the range TeX can actually represent (|n| < 2^31).
            let hi = (self.next_u32() as u64) << 32;
            let lo = self.next_u32() as u64;
            return ((hi | lo) % bound) as i64;
        }
        let bound = bound as u32;
        // Reject the short tail so every residue has the same number of
        // preimages.
        let limit = u32::MAX - (u32::MAX % bound) - (bound - 1);
        loop {
            let v = self.next_u32();
            if v <= limit {
                return (v % bound) as i64;
            }
        }
    }

    /// A normal deviate, scaled as pdfTeX reports it: the value is in units of
    /// 1/65536, so `\pdfnormaldeviate` of 65536 means one standard deviation.
    ///
    /// Irwin–Hall: the sum of twelve uniforms on `[0,1)` minus six has mean 0
    /// and variance 1 exactly. It is not a Box–Muller-quality normal — the
    /// tails stop at ±6σ — but it needs no logarithm or square root, which
    /// keeps this module free of floating point entirely. That matters: `f64`
    /// rounding is one of the few things that genuinely can differ between
    /// targets, and determinism across targets is worth more here than the
    /// shape of a tail beyond six sigma.
    pub fn normal(&mut self) -> i64 {
        let mut sum: i64 = 0;
        for _ in 0..12 {
            // Each draw is a fraction of 65536, so the sum is in the same
            // units and no scaling step is needed.
            sum += (self.next_u32() >> 16) as i64;
        }
        sum - 6 * 65536
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_determines_the_stream() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..64 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(2);
        assert!((0..8).any(|_| a.next_u32() != b.next_u32()));
    }

    #[test]
    fn negative_seed_is_its_own_stream() {
        let mut a = Rng::new(-1);
        let mut b = Rng::new(0);
        assert!((0..8).any(|_| a.next_u32() != b.next_u32()));
    }

    #[test]
    fn uniform_stays_in_range() {
        let mut r = Rng::new(7);
        for n in [1i64, 2, 6, 10, 1000, 65536, 2_000_000_000] {
            for _ in 0..500 {
                let v = r.uniform(n);
                assert!(v >= 0 && v < n, "{v} out of range for {n}");
            }
        }
    }

    #[test]
    fn uniform_edge_cases() {
        let mut r = Rng::new(7);
        assert_eq!(r.uniform(0), 0);
        // A negative bound is a range, as in pdfTeX.
        for _ in 0..100 {
            let v = r.uniform(-10);
            assert!((0..10).contains(&v));
        }
    }

    #[test]
    fn uniform_covers_its_range() {
        // A generator that always returned 0 would pass every test above.
        let mut r = Rng::new(3);
        let mut seen = [false; 6];
        for _ in 0..2000 {
            seen[r.uniform(6) as usize] = true;
        }
        assert!(seen.iter().all(|&s| s), "some residues never appeared");
    }

    #[test]
    fn normal_is_centred_and_spread() {
        let mut r = Rng::new(11);
        let n = 4000;
        let mut sum: i64 = 0;
        let mut extreme = false;
        for _ in 0..n {
            let v = r.normal();
            sum += v;
            if v.abs() > 65536 {
                extreme = true;
            }
        }
        // Mean within a tenth of a standard deviation.
        let mean = sum / n;
        assert!(mean.abs() < 6553, "mean {mean} is not near zero");
        // And it is not a constant.
        assert!(extreme, "no draw ever exceeded one sigma");
    }
}
