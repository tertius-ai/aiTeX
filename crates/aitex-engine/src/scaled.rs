//! TeX's fixed-point arithmetic.
//!
//! Every dimension in TeX is an integer number of *scaled points*: 65536 sp =
//! 1 pt, and the range is bounded at ±(2^30 − 1) sp ≈ ±16383.99998 pt. There is
//! no floating point anywhere in the typesetting path, which is why TeX's output
//! is bit-for-bit reproducible across machines — and why our differential DVI
//! oracle can compare byte-exactly rather than within a tolerance.
//!
//! Reproducing the arithmetic *exactly*, including its rounding and its
//! overflow points, is therefore not pedantry: it is the precondition for the
//! only test oracle we have.

use core::fmt;

/// A dimension in scaled points.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct Scaled(pub i32);

/// One point, in scaled points.
pub const UNITY: i32 = 1 << 16;
/// Largest legal dimension. TeX rejects anything at or beyond this.
pub const MAX_DIMEN: i32 = (1 << 30) - 1;

impl Scaled {
    pub const ZERO: Scaled = Scaled(0);
    pub const MAX: Scaled = Scaled(MAX_DIMEN);
    /// A rule dimension that is "running": taken from the enclosing box.
    ///
    /// `tex.web` calls this `null_flag` and gives it -2^30 rather than a
    /// positive sentinel for a reason worth keeping: packing routines compute
    /// heights with `max`, and a large *negative* running dimension loses every
    /// comparison automatically. A positive sentinel would win them all, making
    /// `\hbox{\vrule}` a box 16383pt tall.
    pub const RUNNING: Scaled = Scaled(-(1 << 30));

    pub const fn from_points(pt: i32) -> Scaled {
        Scaled(pt * UNITY)
    }

    /// True when this exceeds TeX's legal range. TeX reports "Dimension too
    /// large" and clamps rather than wrapping.
    pub const fn is_too_large(self) -> bool {
        self.0 >= MAX_DIMEN || self.0 <= -MAX_DIMEN
    }

    pub const fn clamp_to_legal(self) -> Scaled {
        if self.0 >= MAX_DIMEN {
            Scaled(MAX_DIMEN)
        } else if self.0 <= -MAX_DIMEN {
            Scaled(-MAX_DIMEN)
        } else {
            self
        }
    }

    pub const fn abs(self) -> Scaled {
        Scaled(self.0.abs())
    }
}

/// `xn_over_d` from `tex.web` §107: compute `x * n / d` without overflowing,
/// even when `x * n` would not fit in 32 bits.
///
/// Returns the quotient and the remainder. TeX relies on both; the remainder
/// feeds glue-set calculations.
///
/// The decomposition (`x = t*d + u`, working in halves) is Knuth's, and matters
/// because the naive `(x as i64 * n / d)` disagrees with it on the rounding of
/// negative operands — which would show up as a one-scaled-point drift in DVI
/// output and break byte-exact comparison.
pub fn xn_over_d(x: Scaled, n: i32, d: i32) -> (Scaled, Scaled) {
    debug_assert!(d != 0, "caller must reject division by zero");
    let positive = x.0 >= 0;
    let x_abs = if positive { x.0 } else { -x.0 };

    let t = (x_abs % 32768) * n;
    let mut u = (x_abs / 32768) * n + t / 32768;
    let v = (u % d) * 32768 + t % 32768;
    if u / d >= 32768 {
        // Overflow: TeX reports "Arithmetic overflow" and yields 0.
        return (Scaled(0), Scaled(0));
    }
    u = 32768 * (u / d) + v / d;

    if positive {
        (Scaled(u), Scaled(v % d))
    } else {
        (Scaled(-u), Scaled(-(v % d)))
    }
}

/// `mult_and_add` from `tex.web` §105: `x * n + y`, saturating at `max_answer`
/// instead of wrapping.
pub fn mult_and_add(x: Scaled, n: i32, y: Scaled, max_answer: i32) -> Scaled {
    let (x, n) = if x.0 < 0 { (Scaled(-x.0), -n) } else { (x, n) };
    if n == 0 {
        return y;
    }
    let limit = (max_answer - y.0) / n;
    if x.0 <= limit {
        Scaled(x.0 * n + y.0)
    } else {
        Scaled(max_answer)
    }
}

/// `x_over_n` from `tex.web` §106: `x / n`, with the remainder.
///
/// TeX truncates toward zero and carries the sign on the remainder, which is
/// *not* what Rust's `/` does for negative operands in every case we care
/// about — hence doing it explicitly.
pub fn x_over_n(x: Scaled, n: i32) -> (Scaled, Scaled) {
    if n == 0 {
        return (Scaled(0), x);
    }
    let negative = n < 0;
    let n = if negative { -n } else { n };
    if x.0 >= 0 {
        let q = x.0 / n;
        let r = x.0 % n;
        if negative {
            (Scaled(-q), Scaled(r))
        } else {
            (Scaled(q), Scaled(r))
        }
    } else {
        let q = -((-x.0) / n);
        let r = -((-x.0) % n);
        if negative {
            (Scaled(-q), Scaled(r))
        } else {
            (Scaled(q), Scaled(r))
        }
    }
}

/// Render as TeX prints dimensions: shortest decimal that reads back exactly,
/// always with a trailing digit (`1.0pt`, not `1pt`). `tex.web` §103.
pub fn print_scaled(s: Scaled) -> alloc::string::String {
    use alloc::string::String;
    use core::fmt::Write;

    let mut out = String::new();
    let mut n = s.0;
    if n < 0 {
        out.push('-');
        n = -n;
    }
    let _ = write!(out, "{}", n / UNITY);
    out.push('.');

    // Knuth's digit generation: repeatedly scale the fraction by 10.
    let mut f = 10 * (n % UNITY) + 5;
    let mut delta = 10;
    loop {
        if delta > UNITY {
            // Round to the nearest representable value.
            f += 32768 - delta / 2;
        }
        let _ = write!(out, "{}", f / UNITY);
        f = 10 * (f % UNITY);
        delta *= 10;
        if f <= delta {
            break;
        }
    }
    out
}

impl fmt::Display for Scaled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}pt", print_scaled(*self))
    }
}

impl core::ops::Add for Scaled {
    type Output = Scaled;
    fn add(self, o: Scaled) -> Scaled {
        Scaled(self.0.saturating_add(o.0))
    }
}

impl core::ops::Sub for Scaled {
    type Output = Scaled;
    fn sub(self, o: Scaled) -> Scaled {
        Scaled(self.0.saturating_sub(o.0))
    }
}

impl core::ops::Neg for Scaled {
    type Output = Scaled;
    fn neg(self) -> Scaled {
        Scaled(-self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn one_point_is_65536_scaled_points() {
        assert_eq!(Scaled::from_points(1).0, 65536);
        assert_eq!(UNITY, 65536);
    }

    /// The exact strings TeX itself prints. These are the format DVI and log
    /// comparison depend on, so they are pinned literally.
    #[test]
    fn prints_dimensions_the_way_tex_does() {
        assert_eq!(print_scaled(Scaled::from_points(1)), "1.0");
        assert_eq!(print_scaled(Scaled(0)), "0.0");
        assert_eq!(print_scaled(Scaled(-65536)), "-1.0");
        assert_eq!(print_scaled(Scaled(32768)), "0.5");
        assert_eq!(print_scaled(Scaled(1)), "0.00002");
        assert_eq!(Scaled::from_points(1).to_string(), "1.0pt");
    }

    /// Verified against TeX Live 2024 by `\the\dimen0`. These exact strings are
    /// what the real engine emits, so they pin our formatter to the oracle
    /// rather than to my reading of `tex.web`.
    ///
    /// ```text
    /// 1pt -> 1.0pt   0.5pt -> 0.5pt        1sp -> 0.00002pt
    /// 1in -> 72.26999pt                    1cm -> 28.45274pt
    /// ```
    #[test]
    fn matches_real_tex_output() {
        assert_eq!(print_scaled(Scaled(65536)), "1.0");
        assert_eq!(print_scaled(Scaled(32768)), "0.5");
        assert_eq!(print_scaled(Scaled(1)), "0.00002");
        // 1in = 7227/100 pt; TeX truncates 4736286.72 sp to 4736286.
        assert_eq!(print_scaled(Scaled(4_736_286)), "72.26999");
        // 1cm = 7227/254 pt.
        assert_eq!(print_scaled(Scaled(1_864_679)), "28.45274");
    }

    #[test]
    fn max_dimen_matches_texs_limit() {
        // TeX's largest dimension is 16383.99998pt.
        assert_eq!(print_scaled(Scaled(MAX_DIMEN)), "16383.99998");
        assert!(Scaled(MAX_DIMEN).is_too_large());
        assert!(!Scaled(MAX_DIMEN - 1).is_too_large());
    }

    #[test]
    fn xn_over_d_halves_exactly() {
        let (q, r) = xn_over_d(Scaled::from_points(10), 1, 2);
        assert_eq!(q, Scaled::from_points(5));
        assert_eq!(r, Scaled(0));
    }

    #[test]
    fn xn_over_d_survives_intermediate_overflow() {
        // x*n = 2^30 * 3 overflows i32; Knuth's decomposition must not.
        let x = Scaled(1 << 29);
        let (q, _) = xn_over_d(x, 3, 3);
        assert_eq!(q, x, "x*3/3 must round-trip without overflowing");
    }

    #[test]
    fn xn_over_d_is_sign_symmetric() {
        for v in [1, 7, 65536, 1 << 20] {
            let (qp, rp) = xn_over_d(Scaled(v), 3, 7);
            let (qn, rn) = xn_over_d(Scaled(-v), 3, 7);
            assert_eq!(qp.0, -qn.0, "quotient sign symmetry at {v}");
            assert_eq!(rp.0, -rn.0, "remainder sign symmetry at {v}");
        }
    }

    #[test]
    fn x_over_n_truncates_toward_zero() {
        assert_eq!(x_over_n(Scaled(7), 2).0, Scaled(3));
        assert_eq!(x_over_n(Scaled(-7), 2).0, Scaled(-3));
        assert_eq!(x_over_n(Scaled(7), -2).0, Scaled(-3));
        // Division by zero yields zero and returns the input as remainder.
        assert_eq!(x_over_n(Scaled(7), 0), (Scaled(0), Scaled(7)));
    }

    #[test]
    fn mult_and_add_saturates_instead_of_wrapping() {
        assert_eq!(mult_and_add(Scaled(3), 4, Scaled(5), MAX_DIMEN), Scaled(17));
        let big = Scaled(MAX_DIMEN / 2);
        assert_eq!(
            mult_and_add(big, 100, Scaled(0), MAX_DIMEN),
            Scaled(MAX_DIMEN)
        );
    }

    #[test]
    fn addition_saturates() {
        assert_eq!((Scaled(i32::MAX) + Scaled(1)).0, i32::MAX);
        assert_eq!((Scaled(i32::MIN) - Scaled(1)).0, i32::MIN);
    }

    /// Property: printing then reading back must be lossless for every scaled
    /// value. Deterministic sweep rather than random, so failures reproduce.
    #[test]
    fn print_round_trips_across_the_range() {
        let mut v = 1i64;
        while v < MAX_DIMEN as i64 {
            for cand in [v, v + 1, -v] {
                if cand.unsigned_abs() >= MAX_DIMEN as u64 {
                    continue;
                }
                let s = Scaled(cand as i32);
                let printed = print_scaled(s);
                let parsed: f64 = printed.parse().expect("must parse as a decimal");
                let reconstructed = (parsed * UNITY as f64).round() as i32;
                assert_eq!(
                    reconstructed, s.0,
                    "round-trip failed for {} -> {printed}",
                    s.0
                );
            }
            v = v * 7 / 3 + 1;
        }
    }
}
